//! 桌宠眼里的"现在"。默认跟随系统；设置里选了时区就按那个时区算。
//! 提醒、免打扰时段、节日和告诉模型的时间都走这里。数据库里存的是不分时区的秒数，不受影响。

use chrono::{DateTime, FixedOffset, Local, Utc};
use chrono_tz::Tz;
use std::sync::RwLock;

static ZONE: RwLock<Option<Tz>> = RwLock::new(None);

/// 时区名是否有效（比如 Asia/Tokyo）
pub fn valid(name: &str) -> bool {
    name.parse::<Tz>().is_ok()
}

/// 换时区。None、空字符串和认不出的名字都算跟随系统
pub fn set(name: Option<&str>) {
    let zone = name.map(str::trim).filter(|n| !n.is_empty()).and_then(|n| n.parse::<Tz>().ok());
    *ZONE.write().unwrap() = zone;
}

fn in_zone(zone: Option<Tz>, at: DateTime<Utc>) -> DateTime<FixedOffset> {
    match zone {
        Some(tz) => at.with_timezone(&tz).fixed_offset(),
        None => at.with_timezone(&Local).fixed_offset(),
    }
}

pub fn now() -> DateTime<FixedOffset> {
    in_zone(*ZONE.read().unwrap(), Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn converts_to_the_chosen_zone() {
        let at = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        let tokyo = in_zone("Asia/Tokyo".parse().ok(), at);
        assert_eq!(tokyo.format("%Y-%m-%d %H:%M").to_string(), "2026-01-15 21:00");
        assert_eq!(tokyo.timestamp(), at.timestamp());
    }

    #[test]
    fn follows_daylight_saving() {
        let ny: Option<Tz> = "America/New_York".parse().ok();
        let winter = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        let summer = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();
        assert_eq!(in_zone(ny, winter).format("%H:%M").to_string(), "07:00");
        assert_eq!(in_zone(ny, summer).format("%H:%M").to_string(), "08:00");
    }

    #[test]
    fn checks_zone_names() {
        assert!(valid("Asia/Shanghai") && valid("UTC"));
        assert!(!valid("Mars/Base") && !valid(""));
    }
}
