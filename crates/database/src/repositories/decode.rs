//! 把「某一列的文本解不出来」变成 rusqlite 认识的那种错误。
//!
//! 这个文件存在的唯一理由是：同一个表达式曾在七个地方各写了一遍，用了五个不同的
//! 私有函数名 —— `decode`（activities、executions）、`decode_error`（chat、servers）、
//! `decode_error_with`（servers，只是前者的包装）、`decode_err`（workspaces）、
//! `err`（providers），另外还有两处直接内联（snapshots、providers）。
//!
//! 它们对外都是「同一个仓库里的一件小事」，所以谁也不会去看别人怎么写；于是同一件
//! 事出现了两种签名（`&str` 与 `impl Display`），而 servers.rs 里甚至同时存在
//! 两个名字，其中一个只是把另一个包了一层。
//!
//! 值得统一的理由不只是整洁：这条错误路径决定「JSON 列坏掉时用户看到什么」。
//! 七份实现意味着改一处措辞或换一种错误类型时，会有六处继续用旧写法，而这种不一致
//! 只在数据真的损坏时才显形 —— 那正是最不适合现场排查的时刻。

use rusqlite::types::Type;
use rusqlite::Error;

/// 第 `index` 列的文本无法按预期解释时的错误。
///
/// 参数用 `impl Display` 而不是 `&str`：调用方手里有时是 `serde_json::Error`
/// （见 snapshots.rs），有时是一句自己的说明，两者都该能直接传进来，不必各自
/// `to_string()` 一遍。
///
/// 列类型固定标成 `Type::Text`：这些解码失败全部发生在「从 TEXT 列读出来的字符串
/// 解析不了」这个场景（JSON 列、日期列、枚举列），没有第二种情况。
pub(crate) fn decode_error(index: usize, error: impl std::fmt::Display) -> Error {
    Error::FromSqlConversionFailure(index, Type::Text, error.to_string().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_identifies_the_column_and_carries_the_message() {
        let error = decode_error(7, "expected a JSON object");

        // 位置与类型是这条错误唯一有价值的部分：它要能告诉调用方**哪一列**出了问题，
        // 否则「解不出来」在十几列的表里等于没有信息。
        match error {
            Error::FromSqlConversionFailure(index, kind, message) => {
                assert_eq!(index, 7, "the column index was lost");
                assert_eq!(kind, Type::Text, "the column type was lost");
                assert_eq!(message.to_string(), "expected a JSON object");
            }
            other => panic!("unexpected error shape: {other:?}"),
        }
    }

    #[test]
    fn a_display_error_is_rendered_the_same_way_a_string_would_be() {
        // 两种调用形态（传 &str / 传 serde_json::Error）必须得到同样的消息，
        // 否则同一次解码失败会因为「恰好用了哪个重载」而显示不同文本。
        let from_str = decode_error(0, "boom");
        let from_error: Error = decode_error(0, serde_json::from_str::<i32>("nope").unwrap_err());
        let rendered = |error: Error| match error {
            Error::FromSqlConversionFailure(_, _, message) => message.to_string(),
            other => panic!("unexpected error shape: {other:?}"),
        };
        assert_eq!(rendered(from_str), "boom");
        assert!(
            rendered(from_error).contains("expected ident"),
            "serde's own wording is preserved"
        );
    }
}
