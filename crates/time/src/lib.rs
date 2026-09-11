//! yukinal-time — 时间戳的**唯一**实现。
//!
//! 这个 crate 存在的理由很具体：同一套「取当前秒数 → 转 ISO-8601 UTC」的逻辑
//! 曾在四个地方各写了一遍 ——
//!
//! - `crates/core/src/sidecar.rs`：`iso8601_now` + `now_epoch_seconds` +
//!   `iso8601_utc` + `civil_from_days`
//! - `crates/core/src/collector.rs`：`now_epoch_seconds`
//! - `crates/terminal/src/lib.rs`：`iso8601_now` + `iso8601_utc` + `civil_from_days`
//! - `crates/collector/tests/live.rs`：内联的 `now_epoch_seconds`
//!
//! 其中 `crates/terminal/src/lib.rs` 那份旁边还留着一句注释：「与 core 侧同一套
//! 日历算法（civil_from_days），保持所有时间戳格式一致」—— 作者知道自己在抄，
//! 并把「保持一致」寄托在纪律上。这类纪律的失效方式是静默的：改了一处、忘了另一处，
//! 于是同一份审计记录里出现两种时间格式。
//!
//! 为什么必须是独立 crate 而不是复用某个现有的：`crates/terminal` 依赖面刻意极窄
//! （只有 `yukinal-ssh`），而 `crates/core` 在最上层。任何一方都不可能依赖另一方，
//! 所以共享实现只能放在两者共同的**下层**。仓库里 `crates/filesystem` 只有一个文件，
//! 说明为一件小事开一个 crate 是既有风格。
//!
//! 不引入 `chrono` / `time`：这里只需要一个显示用的字符串，为它拉进一个日期库
//! 不值得（这也是原本 core 侧那句注释的意思）。

#![forbid(unsafe_code)]

/// 当前 Unix 秒。
///
/// 系统时钟早于 1970 时（`duration_since` 返回 `Err`）取 0 而不是 panic：
/// 时间戳是给界面显示的，一个错乱的时钟不该让采集或终端会话直接失败。
#[must_use]
pub fn now_epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_secs())
        .unwrap_or(0)
}

/// 当前时刻的 ISO-8601 UTC 字符串，例如 `2023-11-14T22:13:20Z`。
#[must_use]
pub fn iso8601_now() -> String {
    iso8601_utc(now_epoch_seconds())
}

/// Unix 秒 → ISO-8601 UTC。
///
/// 固定用 `Z`（UTC）而不是本地时区：这些字符串会写进数据库、并跨「Rust 侧写入 /
/// 前端展示」两个环节，本地时区会让同一条记录在不同机器上含义不同。
#[must_use]
pub fn iso8601_utc(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let time_of_day = epoch % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60,
        time_of_day % 60
    )
}

/// 公历日期转换，Howard Hinnant 的 `civil_from_days` 算法。
///
/// 用整数运算而不是查表或日期库：它要能在任何平台上对任何 `u64` 秒数给出同一个
/// 答案，且不依赖时区数据库。
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
                                                                                              // One era is 400 years (146_097 days), not 146_097 years.
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32; // [1, 31]
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 向量由 `new Date(epoch * 1000).toISOString()` 生成，所以它们是**外部**参照，
    /// 不是「跑一遍记下结果」那种自证测试。
    #[test]
    fn iso8601_matches_reference_timestamps() {
        let cases = [
            (0_u64, "1970-01-01T00:00:00Z"),
            (59, "1970-01-01T00:00:59Z"),
            (3_661, "1970-01-01T01:01:01Z"),
            (1_582_934_400, "2020-02-29T00:00:00Z"), // leap day
            (1_700_000_000, "2023-11-14T22:13:20Z"),
            (1_893_456_000, "2030-01-01T00:00:00Z"),
            (2_147_483_647, "2038-01-19T03:14:07Z"), // y2k38 boundary
        ];
        for (epoch, expected) in cases {
            assert_eq!(iso8601_utc(epoch), expected, "epoch {epoch}");
        }
    }

    #[test]
    fn every_month_boundary_of_a_leap_year_holds() {
        // 逐个跨月检查，专门盯住 `month <= 2` 那次 +1 年的进位：一个只在 1 月
        // 和 2 月出错的日期实现，用上面那组向量是看不出来的。
        let cases = [
            (1_704_067_199, "2023-12-31T23:59:59Z"),
            (1_704_067_200, "2024-01-01T00:00:00Z"),
            (1_706_745_600, "2024-02-01T00:00:00Z"),
            (1_709_251_200, "2024-03-01T00:00:00Z"),
            (1_725_148_800, "2024-09-01T00:00:00Z"),
        ];
        for (epoch, expected) in cases {
            assert_eq!(iso8601_utc(epoch), expected, "epoch {epoch}");
        }
    }

    #[test]
    fn the_format_is_fixed_width_so_rows_line_up() {
        // 时间戳在列表里逐行排列，宽度漂移会让整列看起来是歪的。定宽同时保证了
        // ISO-8601 的字典序等于时间序（下面那个测试依赖这一点）。
        for epoch in [0_u64, 999_999_999, 1_700_000_000, 4_102_444_800] {
            let rendered = iso8601_utc(epoch);
            assert_eq!(rendered.len(), 20, "{rendered} is not 20 chars");
            assert!(rendered.ends_with('Z'), "{rendered} is not UTC-marked");
            assert_eq!(
                rendered.as_bytes()[10],
                b'T',
                "{rendered} lost its T separator"
            );
        }
    }

    #[test]
    fn now_sits_between_the_readings_taken_around_it() {
        // 不解析字符串，而是拿它和前后两次读数各自的渲染结果比较。ISO-8601 的
        // 字典序与时间序一致（定宽、同为 UTC），所以字符串比较在这里成立 —— 这也
        // 正是这个格式值得被固定的一个好处。
        let before = now_epoch_seconds();
        let rendered = iso8601_now();
        let after = now_epoch_seconds();

        assert_eq!(rendered.len(), 20, "unexpected shape: {rendered}");
        assert!(rendered.ends_with('Z'), "not UTC-marked: {rendered}");
        assert!(before <= after, "the clock went backwards mid-test");
        assert!(
            rendered.as_str() >= iso8601_utc(before).as_str(),
            "{rendered} predates the reading taken before it"
        );
        assert!(
            rendered.as_str() <= iso8601_utc(after).as_str(),
            "{rendered} postdates the reading taken after it"
        );
    }
}
