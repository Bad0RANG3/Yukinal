//! yukinal-filesystem — 本地与远端文件能力。
//!
//! 两类目标必须严格区分，避免跨环境误操作：
//! - `local`  : Yukinal 所在机器的文件系统（项目仓库、下载目录）。
//! - `remote` : 通过 `yukinal-ssh` 的 SFTP channel 访问的服务器文件系统。
//!
//! Tool 层（`filesystem.list/read/write`）当前为契约占位。
