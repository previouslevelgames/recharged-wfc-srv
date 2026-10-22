//! The game's seasons (`docs/gamestats2.md`, "Seasons"): weekly from fixed
//! dates, and a day for striker of the day. The dates are the game's own, from
//! a row's userdata; the server keeps each player's last row of a season or day
//! that has ended.

use time::{Date, Month};

/// The days a season starts on, as month and day: `sNetworkSeasonDates` in
/// `NetworkSeasonCalendar.cpp`, every week from 1 January.
const STARTS: [(u8, u8); 52] = [
    (1, 1),
    (1, 8),
    (1, 15),
    (1, 22),
    (1, 29),
    (2, 5),
    (2, 12),
    (2, 19),
    (2, 26),
    (3, 5),
    (3, 12),
    (3, 19),
    (3, 26),
    (4, 2),
    (4, 9),
    (4, 16),
    (4, 23),
    (4, 30),
    (5, 7),
    (5, 14),
    (5, 21),
    (5, 28),
    (6, 4),
    (6, 11),
    (6, 18),
    (6, 25),
    (7, 2),
    (7, 9),
    (7, 16),
    (7, 23),
    (7, 30),
    (8, 6),
    (8, 13),
    (8, 20),
    (8, 27),
    (9, 3),
    (9, 10),
    (9, 17),
    (9, 24),
    (10, 1),
    (10, 8),
    (10, 15),
    (10, 22),
    (10, 29),
    (11, 5),
    (11, 12),
    (11, 19),
    (11, 26),
    (12, 3),
    (12, 10),
    (12, 17),
    (12, 24),
];

/// Striker of the day and its EU alternate: their boards start every day.
const DAILY: [i32; 2] = [1, 4];

/// The first day of the season `date` lies in on the board `category`, or the
/// day itself on a daily board.
pub fn first_day(category: i32, date: Date) -> Date {
    if DAILY.contains(&category) {
        return date;
    }
    let day = (u8::from(date.month()), date.day());
    let start = STARTS
        .iter()
        .rev()
        .find(|&&start| start <= day)
        .unwrap_or(&STARTS[0]);
    on(date.year(), *start)
}

/// The last day of the season that starts on `first`, or `first` itself on a
/// daily board.
pub fn last_day(category: i32, first: Date) -> Date {
    if DAILY.contains(&category) {
        return first;
    }
    let day = (u8::from(first.month()), first.day());
    let next = match STARTS.iter().find(|&&start| start > day) {
        Some(&start) => on(first.year(), start),
        None => on(first.year() + 1, STARTS[0]),
    };
    next.previous_day().unwrap_or(next)
}

/// The start `(month, day)` in `year`: every start exists in every year.
fn on(year: i32, (month, day): (u8, u8)) -> Date {
    Month::try_from(month)
        .and_then(|month| Date::from_calendar_date(year, month, day))
        .expect("a season start is a date")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u8, day: u8) -> Date {
        on(year, (month, day))
    }

    #[test]
    fn seasons_start_on_the_games_dates() {
        for (on, starts) in [
            (date(2026, 10, 21), date(2026, 10, 15)),
            (date(2026, 10, 22), date(2026, 10, 22)),
            (date(2026, 1, 1), date(2026, 1, 1)),
            (date(2026, 12, 31), date(2026, 12, 24)),
            // 29 February makes the season of 26 February eight days long.
            (date(2028, 3, 4), date(2028, 2, 26)),
        ] {
            assert_eq!(first_day(0, on), starts, "{on}");
            assert_eq!(first_day(3, on), starts, "{on}");
        }
    }

    #[test]
    fn seasons_end_the_day_before_the_next() {
        assert_eq!(last_day(0, date(2026, 10, 15)), date(2026, 10, 21));
        assert_eq!(last_day(0, date(2026, 12, 24)), date(2026, 12, 31));
        assert_eq!(last_day(0, date(2028, 2, 26)), date(2028, 3, 4));
    }

    #[test]
    fn striker_of_the_day_lasts_a_day() {
        let day = date(2026, 10, 21);
        for category in DAILY {
            assert_eq!(first_day(category, day), day);
            assert_eq!(last_day(category, day), day);
        }
    }
}
