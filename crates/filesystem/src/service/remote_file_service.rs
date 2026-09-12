//! `RemoteFileService`：策略 + 上限 + 有界解码，套在一个 `RemoteFileTransport` 外面。

use crate::limits::{BROWSER_READ_BYTES, MAX_AGENT_EDIT_BYTES};
use crate::revision::content_revision;

use super::error::{Error, Result};
use super::helpers::{byte_match_offsets, count_lines, join_remote_path, read_result};
use super::request::{AgentEditRequest, AgentReadRequest, AgentWriteRequest};
use super::types::{
    RemoteEdit, RemoteEntry, RemoteFileTransport, RemoteListing, RemoteRead, RemoteWrite,
};

/// 远端文件服务：策略 + 上限 + 有界解码，套在一个 [`RemoteFileTransport`] 外面。
pub struct RemoteFileService<T> {
    transport: T,
}

impl<T: RemoteFileTransport> RemoteFileService<T> {
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// 目录列表（UI 浏览器用；Agent 目前没有列表工具）。
    ///
    /// 刻意不套用凭据黑名单，也不做路径形状校验：浏览器的操作者是人，而 Agent 的策略不是
    /// 给人用的（见 crate 文档）。这里唯一生效的规则是「名字归一化成绝对路径」。
    pub async fn list(&self, server_id: &str, path: &str) -> Result<RemoteListing> {
        let entries = self.transport.list(server_id, path).await?;
        Ok(RemoteListing {
            path: path.to_string(),
            entries: entries
                .into_iter()
                .map(|entry| RemoteEntry {
                    path: join_remote_path(path, &entry.name),
                    name: entry.name,
                    file_type: entry.file_type,
                    size: entry.size,
                })
                .collect(),
        })
    }

    /// UI 浏览器的读取：上限固定为 [`BROWSER_READ_BYTES`]，调用方无法要求更多，也不查黑名单。
    ///
    /// 它同样会算出 revision（与 Agent 的读取共用 [`read_result`]，算法只有一份），但命令层不
    /// 发布它：浏览器没有编辑入口，多一个字段只是没人用的契约。
    pub async fn browse_read(&self, server_id: &str, path: &str) -> Result<RemoteRead> {
        let bytes = self
            .transport
            .read_bounded(server_id, path, BROWSER_READ_BYTES)
            .await?;
        Ok(read_result(path, &bytes, BROWSER_READ_BYTES))
    }

    /// Agent `filesystem.read`：请求已经带过策略与上限，这里只做有界读取。
    pub async fn agent_read(
        &self,
        server_id: &str,
        request: &AgentReadRequest,
    ) -> Result<RemoteRead> {
        let bytes = self
            .transport
            .read_bounded(server_id, &request.path, request.max_bytes)
            .await?;
        Ok(read_result(&request.path, &bytes, request.max_bytes))
    }

    /// Agent `filesystem.write`（覆盖写）。
    pub async fn agent_write(
        &self,
        server_id: &str,
        request: &AgentWriteRequest,
    ) -> Result<RemoteWrite> {
        self.transport
            .write(server_id, &request.path, request.content.as_bytes())
            .await?;
        Ok(RemoteWrite {
            path: request.path.clone(),
            bytes_written: request.content.len(),
        })
    }

    /// Agent `filesystem.edit`：一次**有守卫的精确替换**（先读后改）。
    ///
    /// # 顺序
    /// 读全文 → 拒绝超限文件 → 校验 revision → 定位 `oldString`（必须恰好一次）→ 写回。
    /// 每一步的失败都在写回**之前**返回，所以被拒绝的编辑不会碰到文件 —— 测试用假传输的调用
    /// 记录来证明这一点，而不只是看错误类型。
    ///
    /// # 它保证什么
    /// - 只有当文件的内容与 `expectedRevision` 完全一致时才写；
    /// - 只有当 `oldString` 恰好出现一次时才写（零次或多次都会拒绝，让模型重新读、给出更多
    ///   上下文，而不是让它猜）；
    /// - 只有整份文件都读进来了才写（见 [`MAX_AGENT_EDIT_BYTES`]）—— 前缀不足以写回；
    /// - 写回的是原始**字节**：读路径的有损 UTF-8 解码只用于「看一眼」，不参与编辑，所以
    ///   一份非 UTF-8 文件不会被编辑悄悄改写成替换字符。
    ///
    /// # 它不保证什么（必须说清楚）
    /// **这不是原子操作，也不是比较并交换。** SFTP 没有 compare-and-swap，所以这里的顺序
    /// 只能是「检查、然后写」，两次网络往返之间有一个窗口：另一个写入者在这段窗口里改了文件，
    /// 它会被这次写回覆盖，谁都不会知道。窗口被收窄到「读回来的那一份内容必须是刚刚读过的
    /// 那一份」，但没有被关上。真正需要互斥的场景不该靠这个工具来兜底。
    pub async fn agent_edit(
        &self,
        server_id: &str,
        request: &AgentEditRequest,
    ) -> Result<RemoteEdit> {
        let bytes = self
            .transport
            .read_bounded(server_id, &request.path, MAX_AGENT_EDIT_BYTES)
            .await?;
        // 传输用「多给一个字节」表示还有更多（见 `decode_bounded`），所以「超过上限」就是
        // `>`：等于上限的文件是完整的，可以编辑。
        if bytes.len() > MAX_AGENT_EDIT_BYTES {
            return Err(Error::FileTooLargeToEdit {
                limit: MAX_AGENT_EDIT_BYTES,
            });
        }

        let actual = content_revision(&bytes);
        // 大小写不敏感：Agent 可能把 revision 原样抄成大写，那不是「文件变了」。
        if !actual.eq_ignore_ascii_case(&request.expected_revision) {
            return Err(Error::RevisionMismatch {
                expected: request.expected_revision.to_ascii_lowercase(),
                actual,
            });
        }

        let old = request.old_string.as_bytes();
        let start = match byte_match_offsets(&bytes, old).as_slice() {
            [] => {
                return Err(Error::InvalidInput(
                    "oldString was not found in the file; re-read it and pass text that appears in it verbatim"
                        .to_string(),
                ))
            }
            [offset] => *offset,
            matches => {
                return Err(Error::InvalidInput(format!(
                    "oldString occurs {} times in the file; re-read it and include enough surrounding context to make it unique",
                    matches.len()
                )))
            }
        };

        let mut updated = Vec::with_capacity(bytes.len() + request.new_string.len());
        updated.extend_from_slice(&bytes[..start]);
        updated.extend_from_slice(request.new_string.as_bytes());
        updated.extend_from_slice(&bytes[start + old.len()..]);
        // 一次编辑不能产出 `write` 写不了的文件：否则它就成了绕过写入上限的路径。
        if updated.len() > MAX_AGENT_EDIT_BYTES {
            return Err(Error::InvalidInput(format!(
                "the edit would produce a file of {} bytes, over the {MAX_AGENT_EDIT_BYTES}-byte cap for one operation; filesystem.write must be used deliberately instead",
                updated.len()
            )));
        }

        self.transport
            .write(server_id, &request.path, &updated)
            .await?;
        Ok(RemoteEdit {
            path: request.path.clone(),
            revision: content_revision(&updated),
            bytes_before: bytes.len(),
            bytes_after: updated.len(),
            line_delta: count_lines(&updated) - count_lines(&bytes),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::limits::{
        BROWSER_READ_BYTES, DEFAULT_AGENT_READ_BYTES, MAX_AGENT_EDIT_BYTES, MAX_AGENT_READ_BYTES,
        MAX_AGENT_WRITE_BYTES,
    };
    use crate::policy::AGENT_PATH_POLICY_MESSAGE;
    use crate::revision::content_revision;
    use crate::service::{
        AgentEditRequest, AgentReadRequest, AgentWriteRequest, Error, ListedEntry,
        RemoteFileService, RemoteFileTransport, TransportError, TransportResult,
    };

    /// 传输调用记录。用 `Arc` 是因为传输移交给服务之后，测试还要能读到它 ——
    /// 「被拦下的路径没有到达传输」只能靠这份记录证明，光看错误类型看不出来。
    /// 编辑还多一层：它必须证明「被拒绝的编辑没有写」。
    #[derive(Clone, Default)]
    struct CallLog {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl CallLog {
        fn record(&self, call: String) {
            self.calls.lock().expect("call log lock").push(call);
        }

        fn snapshot(&self) -> Vec<String> {
            self.calls.lock().expect("call log lock").clone()
        }

        /// 到目前为止发生过的写次数。编辑的每条拒绝路径都要断言它是 0。
        fn writes(&self) -> usize {
            self.snapshot()
                .iter()
                .filter(|call| call.starts_with("write "))
                .count()
        }
    }

    /// 内存假传输：一个假文件、一次调用记录，可选地失败。
    ///
    /// `read_bounded` 会像真传输那样多给**一个**字节来表示「还有更多」，`write` 会真的替换
    /// 假文件 —— 少了这两个性质，「编辑没有把文件截断」这类断言就没有意义。
    #[derive(Default)]
    struct FakeTransport {
        log: CallLog,
        entries: Vec<ListedEntry>,
        file: Arc<Mutex<Vec<u8>>>,
        failure: Option<&'static str>,
    }

    impl FakeTransport {
        fn with_content(content: &[u8]) -> Self {
            Self {
                file: Arc::new(Mutex::new(content.to_vec())),
                ..Self::default()
            }
        }

        fn log(&self) -> CallLog {
            self.log.clone()
        }

        /// 假文件当前的内容（传输移交之后，测试只能这样看它）。
        fn file(&self) -> Vec<u8> {
            self.file.lock().expect("file lock").clone()
        }
    }

    impl RemoteFileTransport for FakeTransport {
        async fn list(&self, server_id: &str, path: &str) -> TransportResult<Vec<ListedEntry>> {
            self.log.record(format!("list {server_id} {path}"));
            if let Some(message) = self.failure {
                return Err(TransportError::new(message));
            }
            Ok(self.entries.clone())
        }

        async fn read_bounded(
            &self,
            server_id: &str,
            path: &str,
            max_bytes: usize,
        ) -> TransportResult<Vec<u8>> {
            self.log
                .record(format!("read {server_id} {path} @{max_bytes}"));
            if let Some(message) = self.failure {
                return Err(TransportError::new(message));
            }
            let file = self.file();
            // 上限 + 1 是「还有更多」的信号，与 `decode_bounded` 的约定一致。
            let end = file.len().min(max_bytes.saturating_add(1));
            Ok(file[..end].to_vec())
        }

        async fn write(&self, server_id: &str, path: &str, data: &[u8]) -> TransportResult<()> {
            self.log
                .record(format!("write {server_id} {path} {}b", data.len()));
            if let Some(message) = self.failure {
                return Err(TransportError::new(message));
            }
            *self.file.lock().expect("file lock") = data.to_vec();
            Ok(())
        }
    }

    /// revision 的合法形状占位：真正的值由 `content_revision` 算，测试里只关心「它不是
    /// 文件现在的 revision 时会被拒绝」。
    fn other_revision() -> String {
        content_revision(b"some other content")
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().expect("runtime")
    }

    #[test]
    fn listing_normalises_entry_paths_against_the_directory_it_listed() {
        let transport = FakeTransport {
            entries: vec![
                ListedEntry {
                    name: "hosts".to_string(),
                    file_type: "file".to_string(),
                    size: 42,
                },
                ListedEntry {
                    name: "nginx".to_string(),
                    file_type: "dir".to_string(),
                    size: 4_096,
                },
            ],
            ..FakeTransport::default()
        };
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let listing = service.list("srv_1", "/etc").await.expect("list /etc");
            assert_eq!(listing.path, "/etc");
            assert_eq!(listing.entries[0].path, "/etc/hosts");
            assert_eq!(listing.entries[0].file_type, "file");
            assert_eq!(listing.entries[1].path, "/etc/nginx");
            assert_eq!(listing.entries[1].size, 4_096);

            let root = service.list("srv_1", "/").await.expect("list /");
            assert_eq!(root.entries[0].path, "/hosts");

            let trailing = service.list("srv_1", "/etc/").await.expect("list /etc/");
            assert_eq!(trailing.entries[0].path, "/etc/hosts");
        });

        // 归一化发生在服务里，传输只看到调用方给的原文。
        assert_eq!(
            log.snapshot(),
            vec!["list srv_1 /etc", "list srv_1 /", "list srv_1 /etc/"]
        );
    }

    #[test]
    fn an_over_cap_read_request_is_refused_before_the_transport_is_touched() {
        let transport = FakeTransport::with_content(b"PORT=8080\n");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            for max_bytes in [0, MAX_AGENT_READ_BYTES + 1, usize::MAX] {
                match AgentReadRequest::check("/etc/app.env", Some(max_bytes)) {
                    // 这一支在一次通过的运行里不会成立；真成立就会在这里碰到传输，
                    // 下面那次日志断言会立刻失败。
                    Ok(request) => {
                        let _ = service.agent_read("srv_1", &request).await;
                        panic!("maxBytes = {max_bytes} must not yield a request");
                    }
                    Err(error) => assert_eq!(
                        error.to_string(),
                        format!("maxBytes must be between 1 and {MAX_AGENT_READ_BYTES}"),
                        "maxBytes = {max_bytes}"
                    ),
                }
            }
        });

        assert!(
            log.snapshot().is_empty(),
            "a rejected request reached the transport: {:?}",
            log.snapshot()
        );
    }

    #[test]
    fn agent_read_uses_the_documented_default_cap_and_truncates_at_it() {
        let transport = FakeTransport::with_content(b"0123456789abcdefX");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentReadRequest::check("/etc/app.env", None).expect("request");
            let read = service.agent_read("srv_1", &request).await.expect("read");
            assert_eq!(read.path, "/etc/app.env");
            assert_eq!(read.content, "0123456789abcdefX");
            assert!(!read.truncated);
            // 完整读取的 revision 描述整份内容 —— 它正是 `edit` 会拿来比较的那份摘要。
            assert_eq!(read.revision, content_revision(b"0123456789abcdefX"));

            let request = AgentReadRequest::check("/etc/app.env", Some(16)).expect("request");
            let read = service.agent_read("srv_1", &request).await.expect("read");
            assert_eq!(read.content, "0123456789abcdef");
            assert!(read.truncated);
            // 截断读取的 revision 只描述**前缀**（那多出来的第 17 个字节没进正文，也就不算
            // 这次读取的内容），所以它永远不会等于整份文件的 revision。
            assert_eq!(read.revision, content_revision(b"0123456789abcdef"));
            assert_ne!(read.revision, content_revision(b"0123456789abcdefX"));
        });

        // 默认上限是**具体字节数**发给传输的，缺省不在传输里做。
        assert_eq!(
            log.snapshot(),
            vec![
                format!("read srv_1 /etc/app.env @{DEFAULT_AGENT_READ_BYTES}"),
                "read srv_1 /etc/app.env @16".to_string(),
            ]
        );
    }

    #[test]
    fn agent_write_enforces_the_write_cap_and_counts_bytes_not_characters() {
        let transport = FakeTransport::default();
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let at_cap = "x".repeat(MAX_AGENT_WRITE_BYTES);
            let request =
                AgentWriteRequest::check("/var/app/out.txt", at_cap).expect("at-cap request");
            let write = service.agent_write("srv_1", &request).await.expect("write");
            assert_eq!(write.path, "/var/app/out.txt");
            assert_eq!(write.bytes_written, MAX_AGENT_WRITE_BYTES);

            // 上限按字节：两个字符的 "文a" 是 4 字节。
            let request = AgentWriteRequest::check("/var/app/uni.txt", "文a".to_string())
                .expect("unicode request");
            let write = service.agent_write("srv_1", &request).await.expect("write");
            assert_eq!(write.bytes_written, 4);

            let over_cap = "x".repeat(MAX_AGENT_WRITE_BYTES + 1);
            let error = AgentWriteRequest::check("/var/app/out.txt", over_cap)
                .expect_err("over-cap content");
            assert_eq!(
                error.to_string(),
                format!("content must be at most {MAX_AGENT_WRITE_BYTES} bytes")
            );
        });

        assert_eq!(
            log.snapshot(),
            vec![
                format!("write srv_1 /var/app/out.txt {MAX_AGENT_WRITE_BYTES}b"),
                "write srv_1 /var/app/uni.txt 4b".to_string(),
            ]
        );
    }

    #[test]
    fn a_successful_edit_writes_the_replacement_and_returns_the_new_revision() {
        const BEFORE: &[u8] = b"PORT=8080\nHOST=127.0.0.1\n";
        const AFTER_FIRST: &[u8] = b"PORT=9090\nexport PORT\nHOST=127.0.0.1\n";
        const AFTER_SECOND: &[u8] = b"PORT=9090\nexport PORT\nHOST=0.0.0.0\n";
        let transport = FakeTransport::with_content(BEFORE);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/etc/app.env",
                &content_revision(BEFORE),
                "PORT=8080".to_string(),
                "PORT=9090\nexport PORT".to_string(),
            )
            .expect("request");
            let edit = service.agent_edit("srv_1", &request).await.expect("edit");

            assert_eq!(edit.path, "/etc/app.env");
            assert_eq!(edit.bytes_before, BEFORE.len());
            assert_eq!(edit.bytes_after, AFTER_FIRST.len());
            // 行数变化：`newString` 多了一行。
            assert_eq!(edit.line_delta, 1);
            // 返回的 revision 是**写回的那份内容**的摘要：下一次编辑可以直接用它。
            assert_eq!(edit.revision, content_revision(AFTER_FIRST));
            assert_eq!(
                file.lock().expect("file lock").clone(),
                AFTER_FIRST.to_vec(),
                "the transport must have received exactly the replaced bytes"
            );

            // 拿新的 revision 再编辑一次：守卫依赖的正是「编辑返回的 revision 可以继续用」。
            let request = AgentEditRequest::check(
                "/etc/app.env",
                &edit.revision,
                "HOST=127.0.0.1".to_string(),
                "HOST=0.0.0.0".to_string(),
            )
            .expect("second request");
            let second = service.agent_edit("srv_1", &request).await.expect("edit");
            assert_eq!(second.line_delta, 0);
            assert_eq!(
                file.lock().expect("file lock").clone(),
                AFTER_SECOND.to_vec()
            );
        });

        // 恰好一次读 + 一次写，读取用的是编辑上限（传输自己会多给一个字节示意「还有更多」）。
        assert_eq!(
            log.snapshot(),
            vec![
                format!("read srv_1 /etc/app.env @{MAX_AGENT_EDIT_BYTES}"),
                format!("write srv_1 /etc/app.env {}b", AFTER_FIRST.len()),
                format!("read srv_1 /etc/app.env @{MAX_AGENT_EDIT_BYTES}"),
                format!("write srv_1 /etc/app.env {}b", AFTER_SECOND.len()),
            ]
        );
    }

    #[test]
    fn an_edit_is_refused_when_the_file_is_no_longer_the_expected_revision() {
        const BEFORE: &[u8] = b"PORT=8080\n";
        let transport = FakeTransport::with_content(BEFORE);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/etc/app.env",
                &other_revision(),
                "PORT=8080".to_string(),
                "PORT=9090".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(Error::RevisionMismatch { expected, actual }) => {
                    assert_eq!(expected, other_revision());
                    assert_eq!(actual, content_revision(BEFORE));
                }
                other => panic!("expected a revision mismatch, got {other:?}"),
            }
        });

        // 关键的一条：拒绝发生在写之前，而且文件一个字节都没动。
        assert_eq!(
            log.writes(),
            0,
            "a rejected edit wrote: {:?}",
            log.snapshot()
        );
        assert_eq!(file.lock().expect("file lock").clone(), BEFORE.to_vec());
    }

    #[test]
    fn an_edit_whose_read_was_truncated_can_never_pass_the_revision_check() {
        // 这条把「前缀 revision 不能授权编辑」的规则钉在服务层：文件 6000 字节，`read` 只要
        // 了 4096，于是 Agent 手里的 revision 描述的是前 4096 字节。它拿去编辑时，`edit` 读到
        // 的却是一份完整的 6000 字节文件，两者对不上 —— 拒绝，而不是把前 4096 字节写回去。
        let content = vec![b'a'; 6_000];
        let transport = FakeTransport::with_content(&content);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let read_request =
                AgentReadRequest::check("/var/log/app.log", Some(4_096)).expect("read request");
            let read = service
                .agent_read("srv_1", &read_request)
                .await
                .expect("read");
            assert!(read.truncated);

            let request = AgentEditRequest::check(
                "/var/log/app.log",
                &read.revision,
                "aa".to_string(),
                "bb".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(Error::RevisionMismatch { .. }) => {}
                other => panic!("expected a revision mismatch, got {other:?}"),
            }
        });

        assert_eq!(
            log.writes(),
            0,
            "a rejected edit wrote: {:?}",
            log.snapshot()
        );
        assert_eq!(file.lock().expect("file lock").len(), content.len());
    }

    #[test]
    fn a_file_over_the_edit_cap_is_refused_instead_of_being_truncated() {
        // **这是本能力最重要的一条行为。** `read` 的上限是 1 MiB 而编辑上限是 512 KiB，因为
        // 编辑要把整份内容读进来再原样写回：只要文件比能读到的更多，「校验 revision → 写回
        // 缓冲区」就会把用户的文件截成缓冲区那么长。假的传输会像真 SFTP 一样返回上限 + 1 个
        // 字节，所以这里走的是「发现还有更多 → 拒绝」的真实路径。
        let content = vec![b'x'; MAX_AGENT_EDIT_BYTES + 1];
        let transport = FakeTransport::with_content(&content);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/var/log/big.log",
                &content_revision(&content),
                "xx".to_string(),
                "yy".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(error @ Error::FileTooLargeToEdit { limit }) => {
                    assert_eq!(limit, MAX_AGENT_EDIT_BYTES);
                    let message = error.to_string();
                    // 文案必须自己说清两件事：为什么不能编辑，以及该改用哪个工具。
                    assert!(message.contains("larger than"), "{message}");
                    assert!(message.contains("truncate"), "{message}");
                    assert!(message.contains("filesystem.write"), "{message}");
                }
                other => panic!("expected a too-large refusal, got {other:?}"),
            }
        });

        assert_eq!(
            log.writes(),
            0,
            "a refused edit wrote: {:?}",
            log.snapshot()
        );
        assert_eq!(
            file.lock().expect("file lock").len(),
            MAX_AGENT_EDIT_BYTES + 1,
            "the user's file must be untouched"
        );
    }

    #[test]
    fn a_file_exactly_at_the_edit_cap_is_editable() {
        // 边界另一侧：等于上限的文件是**完整**读进来的（「还有更多」的信号是上限 + 1 个字节），
        // 所以它必须可以编辑 —— 否则上限就变成了一个比它宣称的更小的数字。
        let mut content = vec![b'x'; MAX_AGENT_EDIT_BYTES];
        content[MAX_AGENT_EDIT_BYTES - 1] = b'y';
        let transport = FakeTransport::with_content(&content);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/var/log/at-cap.log",
                &content_revision(&content),
                "y".to_string(),
                "z".to_string(),
            )
            .expect("request");
            let edit = service.agent_edit("srv_1", &request).await.expect("edit");
            assert_eq!(edit.bytes_before, MAX_AGENT_EDIT_BYTES);
            assert_eq!(edit.bytes_after, MAX_AGENT_EDIT_BYTES);
            assert_eq!(edit.line_delta, 0);
        });

        assert_eq!(log.writes(), 1);
        let written = file.lock().expect("file lock").clone();
        assert_eq!(written.len(), MAX_AGENT_EDIT_BYTES);
        assert_eq!(written[MAX_AGENT_EDIT_BYTES - 1], b'z');
    }

    #[test]
    fn an_edit_that_would_grow_the_file_over_the_cap_is_refused() {
        // 编辑不能成为绕过写入上限的路径：`newString` 让结果超过上限时拒绝，并且要说清改用
        // `write`。这里 `newString` 自身合法（不超过上限），超限的是**结果**。
        let mut content = vec![b'x'; MAX_AGENT_EDIT_BYTES];
        content[0] = b'y';
        let transport = FakeTransport::with_content(&content);
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/var/log/at-cap.log",
                &content_revision(&content),
                "y".to_string(),
                "y".repeat(MAX_AGENT_EDIT_BYTES),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(Error::InvalidInput(message)) => {
                    assert!(message.contains("filesystem.write"), "{message}");
                }
                other => panic!("expected an over-cap refusal, got {other:?}"),
            }
        });

        assert_eq!(
            log.writes(),
            0,
            "a refused edit wrote: {:?}",
            log.snapshot()
        );
    }

    #[test]
    fn an_absent_or_ambiguous_old_string_is_refused_before_the_write() {
        const BEFORE: &[u8] = b"port=8080\nport=8080\n";
        let transport = FakeTransport::with_content(BEFORE);
        let log = transport.log();
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let revision = content_revision(BEFORE);

            // 出现两次：模型必须重读并给出更多上下文，而不是让工具挑一个来改。
            let ambiguous = AgentEditRequest::check(
                "/etc/app.env",
                &revision,
                "port=8080\n".to_string(),
                "port=9090\n".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &ambiguous).await {
                Err(Error::InvalidInput(message)) => {
                    assert!(message.contains("occurs 2 times"), "{message}");
                    assert!(message.contains("re-read"), "{message}");
                }
                other => panic!("expected an ambiguous-match refusal, got {other:?}"),
            }

            // 一次都没有：同样要重读。
            let absent = AgentEditRequest::check(
                "/etc/app.env",
                &revision,
                "port=3000".to_string(),
                "port=9090".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &absent).await {
                Err(Error::InvalidInput(message)) => {
                    assert!(message.contains("was not found"), "{message}");
                    assert!(message.contains("re-read"), "{message}");
                }
                other => panic!("expected a missing-match refusal, got {other:?}"),
            }

            // 恰好一次：通过。三者的区别只有 `oldString`，所以上面两次拒绝确实是「匹配数」判的。
            let exact = AgentEditRequest::check(
                "/etc/app.env",
                &revision,
                "port=8080\nport=8080\n".to_string(),
                "port=9090\n".to_string(),
            )
            .expect("request");
            let edit = service.agent_edit("srv_1", &exact).await.expect("edit");
            assert_eq!(edit.line_delta, -1);
        });

        assert_eq!(log.writes(), 1, "log: {:?}", log.snapshot());
        assert_eq!(
            file.lock().expect("file lock").clone(),
            b"port=9090\n".to_vec()
        );
    }

    #[test]
    fn an_empty_old_string_is_an_input_problem_not_a_match_problem() {
        // 空串在每份内容里都出现无数次，所以它先被形状校验拒掉，理由说得出话。
        match AgentEditRequest::check(
            "/etc/app.env",
            &content_revision(b"x"),
            String::new(),
            "y".to_string(),
        ) {
            Err(Error::InvalidInput(message)) => {
                assert!(message.contains("oldString must not be empty"), "{message}")
            }
            other => panic!("expected an invalid-input error, got {other:?}"),
        }
    }

    #[test]
    fn a_revision_that_is_not_a_revision_is_refused_before_the_transport_is_touched() {
        // 「Agent 传了半截字符串」必须报成入参问题：如果让它当成 revision 不匹配，模型会被
        // 送去重读一个根本没变的文件。
        let transport = FakeTransport::with_content(b"PORT=8080\n");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let long_z = "z".repeat(64);
            let long_a = "a".repeat(65);
            for revision in ["", "e3b0c442", long_z.as_str(), long_a.as_str()] {
                match AgentEditRequest::check(
                    "/etc/app.env",
                    revision,
                    "PORT=8080".to_string(),
                    "PORT=9090".to_string(),
                ) {
                    Ok(request) => {
                        let _ = service.agent_edit("srv_1", &request).await;
                        panic!("revision {revision:?} must not yield a request");
                    }
                    Err(Error::InvalidInput(message)) => {
                        assert!(message.contains("expectedRevision"), "{message}")
                    }
                    other => panic!("expected an invalid-input error, got {other:?}"),
                }
            }
        });

        assert!(
            log.snapshot().is_empty(),
            "a malformed revision reached the transport: {:?}",
            log.snapshot()
        );
    }

    #[test]
    fn a_revision_compared_case_insensitively_still_matches() {
        // Agent 把 revision 原样抄成大写不是「文件变了」：形状校验收大小写，比较也不应区分。
        const BEFORE: &[u8] = b"PORT=8080\n";
        let transport = FakeTransport::with_content(BEFORE);
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/etc/app.env",
                &content_revision(BEFORE).to_uppercase(),
                "PORT=8080".to_string(),
                "PORT=9090".to_string(),
            )
            .expect("request");
            service.agent_edit("srv_1", &request).await.expect("edit");
        });
    }

    #[test]
    fn an_edit_keeps_non_utf8_bytes_intact() {
        // 有损解码（U+FFFD）是 `read` 的「看一眼」语义，不能带进写路径：一份不是 UTF-8 的文件
        // 被编辑之后，除了被替换的那几个字节之外必须**逐字节**保持原样。
        const BEFORE: &[u8] = b"\xff\xfePORT=8080\x00\n";
        let transport = FakeTransport::with_content(BEFORE);
        let file = transport.file.clone();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/var/lib/app/blob.bin",
                &content_revision(BEFORE),
                "PORT=8080".to_string(),
                "PORT=9090".to_string(),
            )
            .expect("request");
            let edit = service.agent_edit("srv_1", &request).await.expect("edit");
            assert_eq!(edit.bytes_before, BEFORE.len());
        });

        assert_eq!(
            file.lock().expect("file lock").clone(),
            b"\xff\xfePORT=9090\x00\n".to_vec()
        );
    }

    #[test]
    fn an_edit_of_a_new_file_is_a_different_tool() {
        // `edit` 只改已经存在的内容：空文件里 `oldString` 找不到，所以「创建」仍然只能由
        // `write` 完成。这条钉住的是「编辑不是被悄悄扩成写入」。
        let transport = FakeTransport::default();
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentEditRequest::check(
                "/var/app/new.txt",
                &content_revision(b""),
                "anything".to_string(),
                "something".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(Error::InvalidInput(message)) => assert!(message.contains("was not found")),
                other => panic!("expected a missing-match refusal, got {other:?}"),
            }
        });

        assert_eq!(log.writes(), 0, "log: {:?}", log.snapshot());
    }

    #[test]
    fn a_blocked_path_never_reaches_the_transport() {
        let blocked = [
            "/home/deploy/.ssh/id_ed25519",
            "/home/deploy/.aws/credentials",
            "/srv/app/.env.production",
            "/run/secrets/provider-token",
            "/proc/1/environ",
            "/etc/ssl/private/service.key",
            "/etc/shadow",
        ];
        let transport = FakeTransport::with_content(b"secret");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        // 到达传输的唯一途径是一个校验过的请求。这些路径都构造不出请求，所以下面三个
        // `Ok` 分支在一次通过的运行里是死代码 —— 一旦有谁把策略放松了，它会立刻在这里
        // 碰到传输并把路径写进日志，收尾的断言随即失败。
        runtime().block_on(async {
            for path in blocked {
                match AgentReadRequest::check(path, None) {
                    Ok(request) => {
                        let _ = service.agent_read("srv_1", &request).await;
                        panic!("{path} must not yield a read request");
                    }
                    Err(error) => match error {
                        Error::DeniedByPolicy(message) => {
                            assert_eq!(message, AGENT_PATH_POLICY_MESSAGE, "{path}")
                        }
                        other => panic!("{path}: expected a policy denial, got {other:?}"),
                    },
                }
                match AgentWriteRequest::check(path, "x".to_string()) {
                    Ok(request) => {
                        let _ = service.agent_write("srv_1", &request).await;
                        panic!("{path} must not yield a write request");
                    }
                    Err(error) => match error {
                        Error::DeniedByPolicy(message) => {
                            assert_eq!(message, AGENT_PATH_POLICY_MESSAGE, "{path}")
                        }
                        other => panic!("{path}: expected a policy denial, got {other:?}"),
                    },
                }
                // 编辑尤其要拦在这里：它是唯一一个「读一份内容再写回去」的入口，如果黑名单在
                // 别的工具上生效而在这里漏掉，它就成了凭据文件的改写原语。
                match AgentEditRequest::check(
                    path,
                    &content_revision(b"secret"),
                    "old".to_string(),
                    "new".to_string(),
                ) {
                    Ok(request) => {
                        let _ = service.agent_edit("srv_1", &request).await;
                        panic!("{path} must not yield an edit request");
                    }
                    Err(error) => match error {
                        Error::DeniedByPolicy(message) => {
                            assert_eq!(message, AGENT_PATH_POLICY_MESSAGE, "{path}")
                        }
                        other => panic!("{path}: expected a policy denial, got {other:?}"),
                    },
                }
            }
        });

        assert!(
            log.snapshot().is_empty(),
            "a blocked path reached the transport: {:?}",
            log.snapshot()
        );
    }

    #[test]
    fn the_credential_policy_is_decided_before_the_shape_and_limit_checks() {
        // 顺序是对外行为的一部分，宿主侧的失败码依赖它：今天「形状 → 黑名单 → 上限」，
        // 所以带黑名单的路径即使 maxBytes 也非法，报的仍是 denied_by_policy。
        match AgentReadRequest::check("/home/deploy/.ssh/id_rsa", Some(0))
            .expect_err("blocked path with a bad cap")
        {
            Error::DeniedByPolicy(message) => assert_eq!(message, AGENT_PATH_POLICY_MESSAGE),
            other => panic!("expected a policy denial, got {other:?}"),
        }

        match AgentWriteRequest::check("/srv/app/.env", "x".repeat(MAX_AGENT_WRITE_BYTES + 1))
            .expect_err("blocked path with oversized content")
        {
            Error::DeniedByPolicy(message) => assert_eq!(message, AGENT_PATH_POLICY_MESSAGE),
            other => panic!("expected a policy denial, got {other:?}"),
        }

        // 编辑的入参校验顺序同样是「形状 → 黑名单 → 形状细节」：黑名单路径即使 revision
        // 也不合法，报的仍是 denied_by_policy，与另外两个工具一致。
        match AgentEditRequest::check(
            "/srv/app/.env.local",
            "not-a-revision",
            "a".to_string(),
            "b".to_string(),
        )
        .expect_err("blocked path with a malformed revision")
        {
            Error::DeniedByPolicy(message) => assert_eq!(message, AGENT_PATH_POLICY_MESSAGE),
            other => panic!("expected a policy denial, got {other:?}"),
        }

        // 形状校验排在黑名单之前：相对路径先报 invalid_input。
        match AgentReadRequest::check("relative/.env", None).expect_err("relative path") {
            Error::InvalidInput(message) => assert_eq!(message, "remote path must be absolute"),
            other => panic!("expected an invalid-input error, got {other:?}"),
        }
        match AgentEditRequest::check(
            "relative/.env",
            &content_revision(b"x"),
            "a".to_string(),
            "b".to_string(),
        )
        .expect_err("relative path")
        {
            Error::InvalidInput(message) => assert_eq!(message, "remote path must be absolute"),
            other => panic!("expected an invalid-input error, got {other:?}"),
        }
    }

    #[test]
    fn the_ui_browser_read_uses_its_own_cap_and_is_not_policy_blocked() {
        // 浏览器不是 Agent：`~/.ssh/id_rsa` 这类路径由人来打开是正当的。哪一天这里开始拒绝，
        // 说明有人把 Agent 的策略套到了 UI 上 —— 那是行为变更，不是加固。
        let transport = FakeTransport::with_content(b"Host web\n");
        let log = transport.log();
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let read = service
                .browse_read("srv_1", "/home/deploy/.ssh/id_rsa")
                .await
                .expect("browser read");
            assert_eq!(read.path, "/home/deploy/.ssh/id_rsa");
            assert_eq!(read.content, "Host web\n");
            assert!(!read.truncated);

            let listing = service
                .list("srv_1", "/home/deploy/.ssh")
                .await
                .expect("browser list");
            assert_eq!(listing.path, "/home/deploy/.ssh");
        });

        assert_eq!(
            log.snapshot(),
            vec![
                format!("read srv_1 /home/deploy/.ssh/id_rsa @{BROWSER_READ_BYTES}"),
                "list srv_1 /home/deploy/.ssh".to_string(),
            ]
        );
    }

    #[test]
    fn transport_failures_keep_their_own_text() {
        const MESSAGE: &str = "no session cached for server `srv_1`; connect first";
        let transport = FakeTransport {
            failure: Some(MESSAGE),
            ..FakeTransport::default()
        };
        let service = RemoteFileService::new(transport);

        runtime().block_on(async {
            let request = AgentReadRequest::check("/etc/app.env", None).expect("request");
            match service.agent_read("srv_1", &request).await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }

            let request =
                AgentWriteRequest::check("/etc/app.env", "x".to_string()).expect("request");
            match service.agent_write("srv_1", &request).await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }

            // 编辑的第一次传输调用（读取）失败时，原文直接穿到上层，不包装、不改写：宿主侧的
            // `transport_or_cancel` 依赖这段文字判断「连接没了」还是「用户按了停止」。
            let request = AgentEditRequest::check(
                "/etc/app.env",
                &content_revision(b"x"),
                "a".to_string(),
                "b".to_string(),
            )
            .expect("request");
            match service.agent_edit("srv_1", &request).await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }

            match service.browse_read("srv_1", "/etc/app.env").await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }

            match service.list("srv_1", "/etc").await {
                Err(Error::Transport(error)) => assert_eq!(error.to_string(), MESSAGE),
                other => panic!("expected a transport error, got {other:?}"),
            }
        });
    }
}
