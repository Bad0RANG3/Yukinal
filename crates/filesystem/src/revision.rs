//! 内容 revision：`filesystem.read` 返回、`filesystem.edit` 校验的「这份字节」指纹。
//!
//! # 算法与编码
//! **SHA-256**（FIPS 180-4），输出 **64 个小写十六进制字符**（32 字节摘要）。
//!
//! 两侧调用的都是同一个 [`content_revision`]：`read` 用它算返回给 Agent 的 revision，`edit`
//! 用它在写回之前重算一次并比较。同源是这份设计里唯一重要的事 —— 如果两个方向各写一遍
//! 「差不多」的算法，比较就只是在比较两个字符串。
//!
//! # 它描述什么，不描述什么
//! 摘要只覆盖**内容字节**：权限位、属主、mtime 变了 revision 不变。这是刻意的 —— 这个
//! revision 存在的目的是「这份内容还是我读过的那份内容吗」，而不是一个文件身份。
//!
//! 当 `filesystem.read` 报告 `truncated = true` 时，这个摘要描述的是文件的**前缀**（它读到的
//! 那部分）。前缀的 revision 与整份文件的 revision 必然不同，所以它不会通过 `edit` 的校验；
//! 但也正因为如此，`edit` 必须先确认「文件能被完整读进来」，见 [`crate::limits::MAX_AGENT_EDIT_BYTES`]。
//!
//! # 为什么自带实现
//! 这里要的是一个固定算法、一次性、输入已在内存里的摘要：没有流式需求，没有 HMAC 需求，
//! 也没有算法协商。自带 60 行换来的是本 crate 的依赖面不扩大（工作区的规矩是依赖在根
//! `Cargo.toml` 里统一登记并审计，而这里不需要新增一项）。实现由 tests 里的 NIST 向量钉住，
//! 包括多分组与「长度字段落在附加分组」的输入。
//!
//! **这不是签名。** 摘要不是密钥，也不防伪造：能写这个文件的人也能算出任意摘要。它只回答
//! 「自上次读取以来内容变了没有」，这一点在 [`crate::service::RemoteFileService::agent_edit`]
//! 的注释里还有一段更重要的说明（检查和写回之间存在窗口）。

/// 计算一段字节的内容 revision：SHA-256 摘要的小写十六进制表示。
///
/// `read` 与 `edit` 都必须通过这个函数，理由见模块文档。
#[must_use]
pub fn content_revision(bytes: &[u8]) -> String {
    sha256(bytes)
        .iter()
        .map(|word| format!("{word:08x}"))
        .collect()
}

/// revision 的合法形状：64 个十六进制字符（大小写都收，比较时按 ASCII 忽略大小写）。
///
/// 校验形状是为了让「Agent 传了半截字符串」报成 `invalid_input`（可修）而不是「文件变了」
/// （会把它送去重读一个根本没变的文件）。
#[must_use]
pub fn is_content_revision(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// SHA-256 首轮常量：前 32 位小数部分（FIPS 180-4 §4.2.2）。
const ROUND_CONSTANTS: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// 初始哈希值：前 8 个素数平方根的小数部分（FIPS 180-4 §5.3.3）。
const INITIAL_STATE: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// 一个分组的字节数。
const BLOCK_BYTES: usize = 64;

fn sha256(input: &[u8]) -> [u32; 8] {
    // 填充：0x80、若干 0，最后 8 字节是大端的**比特**长度。比特长度用 64 位，所以这里
    // 不需要处理「长度本身溢出」的那条分支（输入先受内存限制）。
    let bit_length = (input.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(input.len() + BLOCK_BYTES * 2);
    padded.extend_from_slice(input);
    padded.push(0x80);
    while padded.len() % BLOCK_BYTES != BLOCK_BYTES - 8 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL_STATE;
    // 按 `step_by` 切而不是 `chunks_exact`：后者的常量分组会被 clippy 建议改用
    // `as_chunks`，而那是比本 crate 声明的 MSRV（1.85）更新的 API。
    for block_start in (0..padded.len()).step_by(BLOCK_BYTES) {
        let block = &padded[block_start..block_start + BLOCK_BYTES];
        let schedule = message_schedule(block);
        compress(&mut state, &schedule);
    }
    state
}

/// 把一个 64 字节分组展开成 64 个 32 位字。
fn message_schedule(block: &[u8]) -> [u32; 64] {
    let mut words = [0u32; 64];
    for (index, word) in words.iter_mut().take(16).enumerate() {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&block[index * 4..index * 4 + 4]);
        *word = u32::from_be_bytes(bytes);
    }
    for index in 16..64 {
        let previous_15 = words[index - 15];
        let previous_2 = words[index - 2];
        let small_sigma_0 =
            previous_15.rotate_right(7) ^ previous_15.rotate_right(18) ^ (previous_15 >> 3);
        let small_sigma_1 =
            previous_2.rotate_right(17) ^ previous_2.rotate_right(19) ^ (previous_2 >> 10);
        words[index] = words[index - 16]
            .wrapping_add(small_sigma_0)
            .wrapping_add(words[index - 7])
            .wrapping_add(small_sigma_1);
    }
    words
}

fn compress(state: &mut [u32; 8], schedule: &[u32; 64]) {
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for index in 0..64 {
        let big_sigma_1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ (!e & g);
        let temp_1 = h
            .wrapping_add(big_sigma_1)
            .wrapping_add(choose)
            .wrapping_add(ROUND_CONSTANTS[index])
            .wrapping_add(schedule[index]);
        let big_sigma_0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let temp_2 = big_sigma_0.wrapping_add(majority);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp_1);
        d = c;
        c = b;
        b = a;
        a = temp_1.wrapping_add(temp_2);
    }
    for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = slot.wrapping_add(value);
    }
}

#[cfg(test)]
mod tests {
    use super::{content_revision, is_content_revision};

    /// NIST / RFC 文档里的标准向量。少一个都不行：填充、分组边界与长度字段是这个实现里
    /// 唯一容易写错的三处，而它们各自只在特定长度的输入上现形。
    #[test]
    fn hashes_match_the_published_sha256_vectors() {
        assert_eq!(
            content_revision(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            content_revision(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            content_revision(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn a_payload_whose_length_field_needs_a_second_block_is_hashed_correctly() {
        // 55 字节时填充正好填满一个分组（长度字段落在同一分组里），56 字节时 0x80 之后
        // 只剩 7 个字节、长度字段必须进下一个分组。这是填充实现最容易差一个字节的地方。
        assert_eq!(
            content_revision(&[b'a'; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            content_revision(&[b'a'; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        // 一千个 'a'：多个分组，且长度字段非零。
        assert_eq!(
            content_revision(&[b'a'; 1_000]),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn a_revision_is_64_lowercase_hex_characters() {
        let revision = content_revision(b"PORT=8080\n");
        assert_eq!(revision.len(), 64);
        assert!(
            revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "{revision}"
        );
        assert!(is_content_revision(&revision));
    }

    #[test]
    fn only_the_content_decides_the_revision() {
        // 同一份字节永远得到同一个 revision（`read` 与 `edit` 的比较才有意义）。
        assert_eq!(content_revision(b"a\nb\n"), content_revision(b"a\nb\n"));
        // 差一个字节就是另一份内容 —— 包括「只是尾部多了换行」这种模型最容易忽略的变化。
        assert_ne!(content_revision(b"a\nb\n"), content_revision(b"a\nb"));
        assert_ne!(content_revision(b"a\nb\n"), content_revision(b"a\nc\n"));
    }

    #[test]
    fn a_revision_shaped_like_something_else_is_not_a_revision() {
        // 形状校验是给「传了半截字符串」用的：它必须是 invalid_input，而不是「文件变了」。
        assert!(!is_content_revision(""));
        assert!(!is_content_revision("e3b0c442"));
        assert!(!is_content_revision(&"z".repeat(64)));
        // 少一位的「几乎正确」同样不合格：长度是形状的一部分。
        assert!(!is_content_revision(&content_revision(b"x")[..63]));
        // 大小写都收：Agent 可能把 revision 原样抄成大写。
        assert!(is_content_revision(
            "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"
        ));
    }
}
