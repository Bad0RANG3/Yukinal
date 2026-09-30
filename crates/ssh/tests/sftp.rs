//! 通过真实 SSH + SFTP 子系统验证文件传输边界。
//!
//! 测试服务器只把虚拟 POSIX 路径映射到本测试的临时目录；真实用户目录和网络都不参与。
//! 这让目录枚举、读写、独占备份与清理走过 russh / SFTP wire protocol，而不是只验证
//! 上层使用的内存 mock。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use russh::keys::{ssh_key, HashAlg};
use russh::server::{Auth, RunningServerHandle, Server as _};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File as SftpEntry, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use yukinal_filesystem::{
    backup_path_for, content_revision, AgentBackupRequest, AgentCleanupBackupRequest,
    AgentEditRequest, AgentRestoreRequest, Error as FileServiceError, ListedEntry, RemoteEntryKind,
    RemoteFileService, RemoteFileTransport, RemoteStat, ReplaceError, ReplaceGuard, ReplacedFile,
    TransportError, TransportResult,
};
use yukinal_ssh::{
    link_count_probe_command, parse_link_count, Authentication, ConnectionSecrets, Error,
    KnownHostsPolicy, OutboundProxy, RusshBackend, Session, SftpClient, SftpEntryKind,
    SftpReplaceError, SftpReplaceGuard, SshBackend, SshConfig,
};

const AUTH_USER: &str = "yukinal-sftp-test";
const AUTH_PASSWORD: &str = "not-a-real-secret";
const SERVER_ID: &str = "srv_sftp_test";

type SshChannels = Arc<Mutex<HashMap<ChannelId, Channel<russh::server::Msg>>>>;

struct TestServer {
    address: SocketAddr,
    fingerprint: String,
    shutdown: RunningServerHandle,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.shutdown("SFTP test complete".to_string());
    }
}

#[derive(Clone)]
struct SftpServer {
    root: PathBuf,
    channels: SshChannels,
}

impl russh::server::Server for SftpServer {
    type Handler = SftpSshHandler;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> Self::Handler {
        SftpSshHandler {
            root: self.root.clone(),
            channels: Arc::clone(&self.channels),
        }
    }
}

struct SftpSshHandler {
    root: PathBuf,
    channels: SshChannels,
}

impl SftpSshHandler {
    async fn take_channel(&self, id: ChannelId) -> Channel<russh::server::Msg> {
        self.channels
            .lock()
            .await
            .remove(&id)
            .expect("opened SFTP channel")
    }
}

impl russh::server::Handler for SftpSshHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == AUTH_USER && password == AUTH_PASSWORD {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel_id: ChannelId,
        data: &[u8],
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        let is_link_count_probe =
            command.starts_with("stat -c %h ") || command.starts_with("stat -f %l ");
        session.channel_success(channel_id)?;
        let handle = session.handle();
        tokio::spawn(async move {
            let (stdout, stderr, exit_status) = if is_link_count_probe {
                let link_count = if command.contains("hardlinked.conf") {
                    b"2\n".as_slice()
                } else if command.contains("unknown-link-count.conf") {
                    b"not a number\n".as_slice()
                } else {
                    b"1\n".as_slice()
                };
                (link_count.to_vec(), Vec::new(), 0)
            } else {
                (
                    Vec::new(),
                    b"unsupported command in SFTP fixture\n".to_vec(),
                    127,
                )
            };
            if !stdout.is_empty() && handle.data(channel_id, stdout).await.is_err() {
                return;
            }
            if !stderr.is_empty() && handle.extended_data(channel_id, 1, stderr).await.is_err() {
                return;
            }
            let _ = handle.exit_status_request(channel_id, exit_status).await;
            let _ = handle.eof(channel_id).await;
            let _ = handle.close(channel_id).await;
        });
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(channel_id)?;
            return Ok(());
        }

        let channel = self.take_channel(channel_id).await;
        session.channel_success(channel_id)?;
        russh_sftp::server::run(
            channel.into_stream(),
            SftpFilesystem::new(self.root.clone()),
        )
        .await;
        Ok(())
    }
}

struct SftpFileTransport<'a> {
    backend: &'a RusshBackend,
    client: &'a SftpClient,
    session: &'a Session,
}

impl RemoteFileTransport for SftpFileTransport<'_> {
    async fn list(&self, _server_id: &str, path: &str) -> TransportResult<Vec<ListedEntry>> {
        self.backend
            .sftp_list_dir_detailed(self.client, path)
            .await
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|(name, file_type, size)| ListedEntry {
                        name,
                        file_type,
                        size,
                    })
                    .collect()
            })
            .map_err(|error| TransportError::new(error.to_string()))
    }

    async fn read_bounded(
        &self,
        _server_id: &str,
        path: &str,
        max_bytes: usize,
    ) -> TransportResult<Vec<u8>> {
        self.backend
            .sftp_read_file_bounded(self.client, path, max_bytes)
            .await
            .map_err(|error| TransportError::new(error.to_string()))
    }

    async fn write(&self, _server_id: &str, path: &str, data: &[u8]) -> TransportResult<()> {
        self.backend
            .sftp_write_file(self.client, path, data)
            .await
            .map_err(|error| TransportError::new(error.to_string()))
    }

    async fn create_exclusive(
        &self,
        _server_id: &str,
        path: &str,
        data: &[u8],
    ) -> TransportResult<()> {
        self.backend
            .sftp_create_file_exclusive(self.client, path, data)
            .await
            .map_err(|error| TransportError::new(error.to_string()))
    }

    async fn remove_file(&self, _server_id: &str, path: &str) -> TransportResult<()> {
        self.backend
            .sftp_remove_file(self.client, path)
            .await
            .map_err(|error| TransportError::new(error.to_string()))
    }

    async fn stat(&self, _server_id: &str, path: &str) -> TransportResult<RemoteStat> {
        self.backend
            .sftp_stat(self.client, path)
            .await
            .map(|stat| RemoteStat {
                kind: match stat.kind {
                    SftpEntryKind::File => RemoteEntryKind::File,
                    SftpEntryKind::Directory => RemoteEntryKind::Directory,
                    SftpEntryKind::Symlink => RemoteEntryKind::Symlink,
                    SftpEntryKind::Other => RemoteEntryKind::Other,
                },
                size: stat.size,
                modified: stat.modified,
            })
            .map_err(|error| TransportError::new(error.to_string()))
    }

    async fn link_count(&self, _server_id: &str, path: &str) -> TransportResult<Option<u64>> {
        let Some(command) = link_count_probe_command(path) else {
            return Ok(None);
        };
        match self
            .backend
            .execute(
                self.session,
                &command,
                Some(Duration::from_secs(2)),
                &CancellationToken::new(),
            )
            .await
        {
            Ok(result) => Ok(parse_link_count(&result.stdout, result.exit_code)),
            // Match TerminalService: a failed probe means "unknown", so the file service refuses.
            Err(_) => Ok(None),
        }
    }

    async fn replace_guarded(
        &self,
        _server_id: &str,
        path: &str,
        guard: &ReplaceGuard,
        data: &[u8],
    ) -> Result<ReplacedFile, ReplaceError> {
        match self
            .backend
            .sftp_replace_file_guarded(
                self.client,
                path,
                SftpReplaceGuard {
                    size: guard.size,
                    modified: guard.modified,
                },
                data,
            )
            .await
        {
            Ok(replaced) => Ok(ReplacedFile {
                size: replaced.size,
                modified: replaced.modified,
            }),
            Err(SftpReplaceError::ConcurrentChange(detail)) => {
                Err(ReplaceError::ConcurrentChange(detail))
            }
            Err(SftpReplaceError::Unsupported(detail)) => Err(ReplaceError::Unsupported(detail)),
            Err(SftpReplaceError::MetadataNotPreserved { message, missing }) => {
                Err(ReplaceError::MetadataNotPreserved { message, missing })
            }
            Err(SftpReplaceError::Transport(error)) => Err(ReplaceError::Transport(
                TransportError::new(error.to_string()),
            )),
        }
    }
}

struct OpenFile {
    remote_path: String,
    file: tokio::fs::File,
}

struct DirectoryCursor {
    remote_path: String,
    entries: Vec<String>,
    read: bool,
}

struct SftpFilesystem {
    root: PathBuf,
    handles: HashMap<String, OpenFile>,
    directories: HashMap<String, DirectoryCursor>,
    metadata: HashMap<String, FileAttributes>,
    next_handle: u64,
}

impl SftpFilesystem {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            handles: HashMap::new(),
            directories: HashMap::new(),
            metadata: HashMap::new(),
            next_handle: 0,
        }
    }

    fn next_handle(&mut self) -> String {
        self.next_handle += 1;
        format!("handle-{}", self.next_handle)
    }

    /// Translate absolute POSIX paths into children of the temporary fixture root.
    /// Reject parent traversal and Windows-specific separators before joining.
    fn resolve(&self, remote_path: &str) -> Result<(String, PathBuf), StatusCode> {
        if !remote_path.starts_with('/') {
            return Err(StatusCode::Failure);
        }

        let mut components = Vec::new();
        for component in remote_path.split('/').skip(1) {
            if component.is_empty() || component == "." {
                continue;
            }
            if component == ".."
                || component.contains('\\')
                || component.contains(':')
                || component.contains('\0')
            {
                return Err(StatusCode::PermissionDenied);
            }
            components.push(component);
        }

        let normalized = if components.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", components.join("/"))
        };
        let mut local_path = self.root.clone();
        for component in components {
            local_path.push(component);
        }
        Ok((normalized, local_path))
    }

    fn join_remote(parent: &str, name: &str) -> String {
        if parent == "/" {
            format!("/{name}")
        } else {
            format!("{parent}/{name}")
        }
    }

    fn file_attributes(&self, remote_path: &str, metadata: &std::fs::Metadata) -> FileAttributes {
        let mut attributes = FileAttributes::from(metadata);
        attributes.size = Some(metadata.len());
        if let Some(saved) = self.metadata.get(remote_path) {
            if saved.uid.is_some() {
                attributes.uid = saved.uid;
            }
            if saved.gid.is_some() {
                attributes.gid = saved.gid;
            }
            if saved.permissions.is_some() {
                attributes.permissions = saved.permissions;
            }
            if saved.atime.is_some() {
                attributes.atime = saved.atime;
            }
            if saved.mtime.is_some() {
                attributes.mtime = saved.mtime;
            }
        }
        attributes
    }

    fn remember_attributes(&mut self, remote_path: String, incoming: FileAttributes) {
        let saved = self.metadata.entry(remote_path).or_default();
        if incoming.uid.is_some() {
            saved.uid = incoming.uid;
        }
        if incoming.user.is_some() {
            saved.user = incoming.user;
        }
        if incoming.gid.is_some() {
            saved.gid = incoming.gid;
        }
        if incoming.group.is_some() {
            saved.group = incoming.group;
        }
        if incoming.permissions.is_some() {
            saved.permissions = incoming.permissions;
        }
        if incoming.atime.is_some() {
            saved.atime = incoming.atime;
        }
        if incoming.mtime.is_some() {
            saved.mtime = incoming.mtime;
        }
    }

    async fn attributes_for(&self, remote_path: &str) -> Result<FileAttributes, StatusCode> {
        let (_, path) = self.resolve(remote_path)?;
        let metadata = tokio::fs::symlink_metadata(path)
            .await
            .map_err(status_from_io)?;
        Ok(self.file_attributes(remote_path, &metadata))
    }
}

impl russh_sftp::server::Handler for SftpFilesystem {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: OpenFlags,
        attributes: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let (remote_path, local_path) = self.resolve(&filename)?;
        let mut options = tokio::fs::OpenOptions::new();
        options
            .read(flags.contains(OpenFlags::READ))
            .write(flags.contains(OpenFlags::WRITE))
            .append(flags.contains(OpenFlags::APPEND));
        if flags.contains(OpenFlags::CREATE) {
            if flags.contains(OpenFlags::EXCLUDE) {
                options.create_new(true);
            } else {
                options.create(true);
            }
        }
        if flags.contains(OpenFlags::TRUNCATE) {
            options.truncate(true);
        }
        let file = options.open(local_path).await.map_err(status_from_io)?;
        if !attributes.is_empty() {
            self.remember_attributes(remote_path.clone(), attributes);
        }
        let handle = self.next_handle();
        self.handles
            .insert(handle.clone(), OpenFile { remote_path, file });
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        let file_closed = self.handles.remove(&handle).is_some();
        let directory_closed = self.directories.remove(&handle).is_some();
        if !file_closed && !directory_closed {
            return Err(StatusCode::Failure);
        }
        Ok(ok_status(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let open = self.handles.get_mut(&handle).ok_or(StatusCode::Failure)?;
        open.file
            .seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(status_from_io)?;
        let mut data = vec![0; len as usize];
        let read = open.file.read(&mut data).await.map_err(status_from_io)?;
        if read == 0 {
            return Err(StatusCode::Eof);
        }
        data.truncate(read);
        Ok(Data { id, data })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let open = self.handles.get_mut(&handle).ok_or(StatusCode::Failure)?;
        open.file
            .seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(status_from_io)?;
        open.file.write_all(&data).await.map_err(status_from_io)?;
        Ok(ok_status(id))
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let open = self.handles.get(&handle).ok_or(StatusCode::Failure)?;
        let metadata = open.file.metadata().await.map_err(status_from_io)?;
        Ok(Attrs {
            id,
            attrs: self.file_attributes(&open.remote_path, &metadata),
        })
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let remote_path = self
            .handles
            .get(&handle)
            .ok_or(StatusCode::Failure)?
            .remote_path
            .clone();
        self.remember_attributes(remote_path, attrs);
        Ok(ok_status(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let (remote_path, local_path) = self.resolve(&path)?;
        let mut reader = tokio::fs::read_dir(local_path)
            .await
            .map_err(status_from_io)?;
        let mut entries = Vec::new();
        while let Some(entry) = reader.next_entry().await.map_err(status_from_io)? {
            entries.push(entry.file_name().to_string_lossy().into_owned());
        }
        entries.sort();

        let handle = self.next_handle();
        self.directories.insert(
            handle.clone(),
            DirectoryCursor {
                remote_path,
                entries,
                read: false,
            },
        );
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let (remote_path, entries) = {
            let directory = self
                .directories
                .get_mut(&handle)
                .ok_or(StatusCode::Failure)?;
            if directory.read {
                return Err(StatusCode::Eof);
            }
            directory.read = true;
            let entries = std::mem::take(&mut directory.entries);
            (directory.remote_path.clone(), entries)
        };

        let mut files = Vec::with_capacity(entries.len());
        for filename in entries {
            let child_remote = Self::join_remote(&remote_path, &filename);
            let (_, child_path) = self.resolve(&child_remote)?;
            let metadata = tokio::fs::symlink_metadata(child_path)
                .await
                .map_err(status_from_io)?;
            files.push(SftpEntry::new(
                filename,
                self.file_attributes(&child_remote, &metadata),
            ));
        }
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let (remote_path, local_path) = self.resolve(&filename)?;
        tokio::fs::remove_file(local_path)
            .await
            .map_err(status_from_io)?;
        self.metadata.remove(&remote_path);
        Ok(ok_status(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let (remote_path, _) = self.resolve(&path)?;
        Ok(Name {
            id,
            files: vec![SftpEntry::dummy(remote_path)],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let remote_path = self.resolve(&path)?.0;
        Ok(Attrs {
            id,
            attrs: self.attributes_for(&remote_path).await?,
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let remote_path = self.resolve(&path)?.0;
        Ok(Attrs {
            id,
            attrs: self.attributes_for(&remote_path).await?,
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let remote_path = self.resolve(&path)?.0;
        self.remember_attributes(remote_path, attrs);
        Ok(ok_status(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        let (old_remote, old_local) = self.resolve(&oldpath)?;
        let (new_remote, new_local) = self.resolve(&newpath)?;
        tokio::fs::rename(old_local, new_local)
            .await
            .map_err(status_from_io)?;
        if let Some(attributes) = self.metadata.remove(&old_remote) {
            self.metadata.insert(new_remote, attributes);
        }
        Ok(ok_status(id))
    }
}

fn ok_status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_string(),
        language_tag: "en-US".to_string(),
    }
}

fn status_from_io(error: std::io::Error) -> StatusCode {
    match error.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

async fn start_server(root: &Path) -> TestServer {
    let host_key = ssh_key::PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519)
        .expect("generate test host key");
    let fingerprint = host_key
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        auth_rejection_time: Duration::from_millis(10),
        inactivity_timeout: None,
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind SSH server");
    let address = listener.local_addr().expect("SSH server address");
    let channels = Arc::new(Mutex::new(HashMap::new()));
    let server_root = root.to_path_buf();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let mut server = SftpServer {
            root: server_root,
            channels,
        };
        let running = server.run_on_socket(config, &listener);
        ready_tx.send(running.handle()).ok();
        let _ = running.await;
    });

    TestServer {
        address,
        fingerprint,
        shutdown: ready_rx.await.expect("SFTP server started"),
    }
}

#[tokio::test]
async fn sftp_subsystem_lists_reads_writes_exclusive_backups_and_removes_files() {
    let remote_root = tempfile::tempdir().expect("isolated remote fixture");
    let remote_etc = remote_root.path().join("etc");
    std::fs::create_dir_all(&remote_etc).expect("create fixture directory");
    let source_path = remote_etc.join("yukinal.conf");
    let source_bytes = b"before config\n";
    std::fs::write(&source_path, source_bytes).expect("create source file");

    let server = start_server(remote_root.path()).await;
    let app_data = tempfile::tempdir().expect("isolated local SSH data");
    let backend = RusshBackend::from_data_dir(app_data.path()).expect("SSH backend");
    backend
        .trust_host("127.0.0.1", server.address.port(), &server.fingerprint)
        .expect("pin generated test server key");
    let session = backend
        .connect(
            SshConfig {
                server_id: "srv_sftp_test".into(),
                host: "127.0.0.1".into(),
                port: server.address.port(),
                username: AUTH_USER.into(),
                authentication: Authentication::Password {
                    credential_ref: "keychain://ssh/sftp-test".into(),
                },
                host_certificate_authority: None,
                known_hosts_policy: KnownHostsPolicy::RequireMatch,
                outbound_proxy: OutboundProxy::default(),
                keepalive_interval_secs: 0,
            },
            ConnectionSecrets {
                password: Some(AUTH_PASSWORD.into()),
                ..ConnectionSecrets::empty()
            },
        )
        .await
        .expect("authenticate to pinned loopback server");
    let sftp = backend.sftp(&session).await.expect("start SFTP subsystem");

    let listing = backend
        .sftp_list_dir_detailed(&sftp, "/etc")
        .await
        .expect("list remote fixture directory");
    assert_eq!(listing.len(), 1);
    assert_eq!(listing[0].0, "yukinal.conf");
    assert_eq!(listing[0].1, "file");
    assert_eq!(listing[0].2, source_bytes.len() as u64);

    let bounded = backend
        .sftp_read_file_bounded(&sftp, "/etc/yukinal.conf", 3)
        .await
        .expect("bounded read");
    assert_eq!(
        bounded, b"befo",
        "one extra byte is retained as the truncation sentinel"
    );
    let original = backend
        .sftp_read_file(&sftp, "/etc/yukinal.conf")
        .await
        .expect("read original file");
    assert_eq!(original, source_bytes);

    let backup_path = "/etc/yukinal.conf.yukinal-backup";
    backend
        .sftp_create_file_exclusive(&sftp, backup_path, &original)
        .await
        .expect("create a new backup without replacement");
    assert_eq!(
        backend
            .sftp_read_file(&sftp, backup_path)
            .await
            .expect("read backup"),
        source_bytes
    );
    let duplicate = backend
        .sftp_create_file_exclusive(&sftp, backup_path, b"must not replace the backup")
        .await
        .expect_err("an existing backup path must not be overwritten");
    assert!(matches!(duplicate, Error::Channel(_)), "got {duplicate:?}");
    assert_eq!(
        backend
            .sftp_read_file(&sftp, backup_path)
            .await
            .expect("verify backup"),
        source_bytes
    );

    backend
        .sftp_write_file(&sftp, "/etc/generated.conf", b"generated\n")
        .await
        .expect("write a new remote file");
    assert_eq!(
        backend
            .sftp_read_file(&sftp, "/etc/generated.conf")
            .await
            .expect("read generated file"),
        b"generated\n"
    );

    backend
        .sftp_remove_file(&sftp, backup_path)
        .await
        .expect("remove only the disposable backup");
    assert!(backend.sftp_stat(&sftp, backup_path).await.is_err());
    assert_eq!(
        std::fs::read(&source_path).expect("verify source fixture was untouched"),
        source_bytes
    );

    drop(sftp);
    backend.close(&session).await.expect("close SSH session");
}

#[tokio::test]
async fn file_service_backup_edit_and_restore_use_guarded_sftp_operations() {
    const ORIGINAL: &[u8] = b"before config\n";
    const EDITED: &[u8] = b"bad config\n";
    const LATER_CHANGE: &[u8] = b"changed after restore\n";
    let remote_root = tempfile::tempdir().expect("isolated remote fixture");
    let remote_etc = remote_root.path().join("etc");
    std::fs::create_dir_all(&remote_etc).expect("create fixture directory");
    let source_path = remote_etc.join("yukinal.conf");
    std::fs::write(&source_path, ORIGINAL).expect("create source file");
    std::fs::write(remote_etc.join("hardlinked.conf"), b"shared bytes\n")
        .expect("create hard-link probe fixture");
    std::fs::write(
        remote_etc.join("unknown-link-count.conf"),
        b"unknown bytes\n",
    )
    .expect("create unknown-link-count fixture");

    let server = start_server(remote_root.path()).await;
    let app_data = tempfile::tempdir().expect("isolated local SSH data");
    let backend = RusshBackend::from_data_dir(app_data.path()).expect("SSH backend");
    backend
        .trust_host("127.0.0.1", server.address.port(), &server.fingerprint)
        .expect("pin generated test server key");
    let session = backend
        .connect(
            SshConfig {
                server_id: SERVER_ID.into(),
                host: "127.0.0.1".into(),
                port: server.address.port(),
                username: AUTH_USER.into(),
                authentication: Authentication::Password {
                    credential_ref: "keychain://ssh/sftp-test".into(),
                },
                host_certificate_authority: None,
                known_hosts_policy: KnownHostsPolicy::RequireMatch,
                outbound_proxy: OutboundProxy::default(),
                keepalive_interval_secs: 0,
            },
            ConnectionSecrets {
                password: Some(AUTH_PASSWORD.into()),
                ..ConnectionSecrets::empty()
            },
        )
        .await
        .expect("authenticate to pinned loopback server");
    let sftp = backend.sftp(&session).await.expect("start SFTP subsystem");
    let service = RemoteFileService::new(SftpFileTransport {
        backend: &backend,
        client: &sftp,
        session: &session,
    });
    let target_path = "/etc/yukinal.conf";

    for (path, token) in [
        ("/etc/hardlinked.conf", "11111111111111111111111111111111"),
        (
            "/etc/unknown-link-count.conf",
            "22222222222222222222222222222222",
        ),
    ] {
        let request = AgentBackupRequest::check(path, token).expect("valid safety probe request");
        let refusal = service
            .agent_backup(SERVER_ID, &request)
            .await
            .expect_err("backup must fail closed unless exactly one link is confirmed");
        assert!(
            matches!(refusal, FileServiceError::UnsafeRemoteWrite(_)),
            "got {refusal:?} for {path}"
        );
        let derived_path = backup_path_for(path, token).expect("derived backup path");
        assert!(
            backend.sftp_stat(&sftp, &derived_path).await.is_err(),
            "a refused backup must not create {derived_path}"
        );
    }

    for (path, contents, old_string) in [
        (
            "/etc/hardlinked.conf",
            b"shared bytes\n".as_slice(),
            "shared",
        ),
        (
            "/etc/unknown-link-count.conf",
            b"unknown bytes\n".as_slice(),
            "unknown",
        ),
    ] {
        let request = AgentEditRequest::check(
            path,
            &content_revision(contents),
            old_string.into(),
            "edited".into(),
        )
        .expect("valid safety probe edit request");
        let refusal = service
            .agent_edit(SERVER_ID, &request)
            .await
            .expect_err("edit must fail closed unless exactly one link is confirmed");
        assert!(
            matches!(refusal, FileServiceError::UnsafeRemoteWrite(_)),
            "got {refusal:?} for {path}"
        );
        assert_eq!(
            backend
                .sftp_read_file(&sftp, path)
                .await
                .expect("read refused target"),
            contents,
            "a refused edit must leave {path} unchanged"
        );
    }

    let backup_request = AgentBackupRequest::check(target_path, "0123456789abcdef0123456789abcdef")
        .expect("valid backup request");

    let backup = service
        .agent_backup(SERVER_ID, &backup_request)
        .await
        .expect("create a real SFTP backup");
    assert_eq!(backup.revision, content_revision(ORIGINAL));
    assert_eq!(backup.bytes_backed_up, ORIGINAL.len());
    assert_eq!(
        backend
            .sftp_read_file(&sftp, &backup.backup_path)
            .await
            .expect("read host-owned backup"),
        ORIGINAL
    );

    let collision = service
        .agent_backup(SERVER_ID, &backup_request)
        .await
        .expect_err("the same backup token must not overwrite an existing recovery copy");
    assert!(
        matches!(collision, FileServiceError::Transport(_)),
        "got {collision:?}"
    );
    assert_eq!(
        backend
            .sftp_read_file(&sftp, &backup.backup_path)
            .await
            .expect("verify the original recovery copy survived"),
        ORIGINAL
    );

    let edit_request =
        AgentEditRequest::check(target_path, &backup.revision, "before".into(), "bad".into())
            .expect("valid guarded edit request");
    let edit = service
        .agent_edit(SERVER_ID, &edit_request)
        .await
        .expect("publish edit through staging and SFTP rename");
    assert_eq!(edit.bytes_before, ORIGINAL.len());
    assert_eq!(edit.bytes_after, EDITED.len());
    assert_eq!(edit.revision, content_revision(EDITED));
    assert_eq!(
        std::fs::read(&source_path).expect("read edited remote fixture"),
        EDITED
    );

    let stale_restore =
        AgentRestoreRequest::check(target_path, &backup.backup_path, &backup.revision)
            .expect("well-formed but stale restore");
    let stale_error = service
        .agent_restore(SERVER_ID, &stale_restore)
        .await
        .expect_err("the pre-edit revision must not authorize a restore over the edit");
    assert!(
        matches!(stale_error, FileServiceError::RevisionMismatch { .. }),
        "got {stale_error:?}"
    );
    assert_eq!(
        std::fs::read(&source_path).expect("stale restore left the target alone"),
        EDITED
    );

    let restore_request =
        AgentRestoreRequest::check(target_path, &backup.backup_path, &edit.revision)
            .expect("restore request uses the current edited revision");
    let restored = service
        .agent_restore(SERVER_ID, &restore_request)
        .await
        .expect("restore through a guarded SFTP replacement");
    assert_eq!(restored.revision, backup.revision);
    assert_eq!(restored.bytes_before, EDITED.len());
    assert_eq!(restored.bytes_after, ORIGINAL.len());
    assert_eq!(
        std::fs::read(&source_path).expect("verify restored remote file"),
        ORIGINAL
    );

    let write_request = yukinal_filesystem::AgentWriteRequest::check(
        target_path,
        String::from_utf8_lossy(LATER_CHANGE).into_owned(),
    )
    .expect("valid later change");
    service
        .agent_write(SERVER_ID, &write_request)
        .await
        .expect("simulate a later external write");
    let stale_after_restore =
        AgentRestoreRequest::check(target_path, &backup.backup_path, &backup.revision)
            .expect("well-formed stale revision");
    let stale_error = service
        .agent_restore(SERVER_ID, &stale_after_restore)
        .await
        .expect_err("a later write must invalidate the previous restore authorization");
    assert!(
        matches!(stale_error, FileServiceError::RevisionMismatch { .. }),
        "got {stale_error:?}"
    );
    assert_eq!(
        std::fs::read(&source_path).expect("later change survived the stale restore"),
        LATER_CHANGE
    );

    let cleanup =
        AgentCleanupBackupRequest::check(target_path, &backup.backup_path, &backup.revision)
            .expect("cleanup is bound to the original backup revision");
    service
        .agent_cleanup_backup(SERVER_ID, &cleanup)
        .await
        .expect("clean up only the verified host-owned backup");
    assert!(backend.sftp_stat(&sftp, &backup.backup_path).await.is_err());
    assert_eq!(
        std::fs::read(&source_path).expect("cleanup does not alter the target"),
        LATER_CHANGE
    );

    backend.close(&session).await.expect("close SSH session");
}
