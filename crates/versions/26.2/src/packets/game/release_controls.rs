use super::*;

fn hex(value: &str) -> Vec<u8> {
    value.as_bytes().chunks_exact(2).map(|pair| {
        u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()
    }).collect()
}

#[test]
fn spawn_mode_optional_varint_preserves_absence_and_present_survival() {
    for (version, bytes, expected) in [
        (776, vec![2, 255], (2, -1)),
        (777, vec![2, 0], (2, -1)),
        (777, vec![2, 1], (2, 0)),
        (777, vec![2, 4], (2, 3)),
        (777, vec![0x81, 1, 0x82, 1], (0, 0)),
    ] {
        let mut reader = Reader::new(&bytes);
        assert_eq!(read_spawn_modes(&mut reader, Ctx { version }).unwrap(), expected);
        reader.ensure_empty().unwrap();
    }
    let mut writer = Writer::default();
    write_spawn_modes(&mut writer, Ctx { version: 777 }, 2, 3);
    assert_eq!(writer.into_vec(), [2, 4]);
}

#[test]
fn respawn_modes_select_bytes_without_misaligning_tail() {
    for (version, literal) in [
        (776, "81010a746573743a776f726c640123456789abcdef02ff01000081013f03"),
        (777, "81010a746573743a776f726c640123456789abcdef020001000081013f03"),
    ] {
        let bytes = hex(literal);
        let mut reader = Reader::new(&bytes);
        let body = Respawn::decode(&mut reader, Ctx { version }).unwrap();
        reader.ensure_empty().unwrap();
        assert_eq!((body.dimension_type, body.game_type, body.previous_game_type), (129, 2, -1));
        assert_eq!(body.dimension, "test:world");
        assert_eq!((body.is_debug, body.is_flat, body.portal_cooldown, body.sea_level, body.data_to_keep),
            (true, false, 129, 63, 3));
        let mut writer = Writer::default();
        body.encode(&mut writer, Ctx { version }).unwrap();
        assert_eq!(writer.into_vec(), bytes);
        assert!(Respawn::decode(&mut Reader::new(&bytes[..bytes.len() - 1]), Ctx { version }).is_err());
    }
}

#[test]
fn login_modes_select_bytes_and_decode_the_spawn_tail() {
    for (version, literal) in [
        (776, "0000012300010a746573743a776f726c648101070300010181010a746573743a776f726c640123456789abcdef02ff01000081013f0000"),
        (777, "0000012300010a746573743a776f726c648101070300010181010a746573743a776f726c640123456789abcdef020001000081013f0000"),
    ] {
        let bytes = hex(literal);
        let body = GameLogin::decode(&mut Reader::new(&bytes), Ctx { version }).unwrap();
        assert_eq!((body.entity_id, body.max_players, body.dimension_type), (291, 129, 129));
        assert_eq!((body.game_type, body.previous_game_type, body.is_debug, body.is_flat), (2, -1, true, false));
        assert_eq!((&body.last_death_location, body.portal_cooldown, body.sea_level), (&None, 129, 63));
        let mut writer = Writer::default();
        body.encode(&mut writer, Ctx { version }).unwrap();
        assert_eq!(writer.into_vec(), bytes);
    }
}
