pub const MAX_DIAL_POSITION: u8 = 15;

/// Convert a dial position to (game_volume, chat_volume), each in 0.0..=1.0.
///
/// At center: both 1.0
/// Toward 0 (chat end): game decreases, chat stays 1.0
/// Toward max (game end): game stays 1.0, chat decreases
pub fn dial_to_volumes(position: u8, max_position: u8) -> (f64, f64) {
    if max_position == 0 {
        return (1.0, 1.0);
    }

    let mid = max_position as f64 / 2.0;
    let pos = position as f64;

    let game_vol = if pos < mid {
        pos / mid
    } else {
        1.0
    };

    let chat_vol = if pos > mid {
        (max_position as f64 - pos) / (max_position as f64 - mid)
    } else {
        1.0
    };

    (game_vol, chat_vol)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_position_both_full() {
        let (game, chat) = dial_to_volumes(7, 15);
        assert!((game - 1.0).abs() < 0.1); // slightly below center
        assert!((chat - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn full_chat_end() {
        let (game, chat) = dial_to_volumes(0, 15);
        assert!((game - 0.0).abs() < f64::EPSILON);
        assert!((chat - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn full_game_end() {
        let (game, chat) = dial_to_volumes(15, 15);
        assert!((game - 1.0).abs() < f64::EPSILON);
        assert!((chat - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn quarter_chat() {
        let (game, chat) = dial_to_volumes(3, 15);
        let expected_game = 3.0 / 7.5;
        assert!((game - expected_game).abs() < 0.01);
        assert!((chat - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn quarter_game() {
        let (game, chat) = dial_to_volumes(12, 15);
        let expected_chat = 3.0 / 7.5;
        assert!((game - 1.0).abs() < f64::EPSILON);
        assert!((chat - expected_chat).abs() < 0.01);
    }
}
