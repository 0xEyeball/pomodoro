//! The locale's first day of the week (glibc), as GtkCalendar determines it.

use chrono::Weekday;

#[cfg(target_os = "linux")]
pub fn first_weekday() -> Weekday {
    use std::ffi::{c_char, c_int};

    extern "C" {
        fn nl_langinfo(item: c_int) -> *const c_char;
    }
    const NL_TIME_WEEK_1STDAY: c_int = 0x20066;
    const NL_TIME_FIRST_WEEKDAY: c_int = 0x20068;

    // SAFETY: GTK has already called setlocale(); both items are defined by glibc. The first
    // returns an integer packed in the pointer value, the second a pointer to a single byte.
    let (origin, first) = unsafe {
        let origin = nl_langinfo(NL_TIME_WEEK_1STDAY) as usize as u32;
        let p = nl_langinfo(NL_TIME_FIRST_WEEKDAY);
        (origin, if p.is_null() { 2 } else { *p as u8 })
    };
    let week_1stday = match origin {
        19971130 => 0, // Sunday
        19971201 => 1, // Monday
        _ => return Weekday::Mon,
    };
    // 0 = Sunday … 6 = Saturday, converted to chrono's Monday-based numbering.
    let sunday_based = (week_1stday + first as u32 + 6) % 7;
    Weekday::try_from(((sunday_based + 6) % 7) as u8).unwrap_or(Weekday::Mon)
}

#[cfg(not(target_os = "linux"))]
pub fn first_weekday() -> Weekday {
    Weekday::Mon
}
