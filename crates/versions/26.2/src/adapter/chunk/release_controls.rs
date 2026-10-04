use super::*;
use lodestone_data::GameDataVersion;

fn hex(value: &str) -> Vec<u8> {
    value.as_bytes().chunks_exact(2).map(|pair| {
        u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()
    }).collect()
}

fn context(registries: &ClientRegistries) -> StackCodecContext<'_> {
    StackCodecContext::new(ProtocolDialect::v26_2().with_game_data_version(GameDataVersion::V26_3), registries)
}

#[test]
fn latest_particle_prefix_keeps_shifted_state_and_vector_speed() {
    let registries = ClientRegistries::default();
    let context = context(&registries);
    let bytes = hex("01e2610100c0372000000000004050e00000000000405d7000000000003e000000bf4000003fc000003e800000bf0000003fa00000810102");
    let directives = decode_latest_particles(&bytes, &context).unwrap();
    assert!(matches!(&directives[..], [Directive::Emit(ClientEvent::Particles {
        speed, distribution: lodestone_model::ParticleDistribution::AlternativeWithSpeed,
        count: 129, options: ParticleOptions::BlockState { state }, ..
    })] if state.raw() == 10771 && *speed == [0.25, -0.5, 1.25]));
    assert!(decode_latest_particles(&bytes[..bytes.len() - 1], &context).is_err());
    let mut extended = bytes;
    extended.push(0);
    assert!(decode_latest_particles(&extended, &context).is_err());
}

#[test]
fn latest_explosion_sound_flag_follows_complete_weighted_options() {
    let registries = ClientRegistries::default();
    let context = context(&registries);
    let mut bytes = hex("c0372000000000004050e00000000000405d700000000000409000000000002100000006746573743a78000101e2613e8000003f4000000300");
    let silent = decode_explode(&bytes, &context).unwrap();
    assert_eq!(silent.len(), 2);
    assert!(matches!(&silent[..], [Directive::Emit(ClientEvent::Explosion { radius, .. }),
        Directive::Emit(ClientEvent::Particles { .. })] if *radius == 4.5));
    *bytes.last_mut().unwrap() = 1;
    let audible = decode_explode(&bytes, &context).unwrap();
    assert!(matches!(&audible[2], Directive::Emit(ClientEvent::Sound { sound, volume, .. })
        if sound.to_string() == "test:x" && *volume == 4.0));
    assert!(decode_explode(&bytes[..bytes.len() - 1], &context).is_err());
    bytes.push(0);
    assert!(decode_explode(&bytes, &context).is_err());
}

#[test]
fn item_particle_consumes_template_patch_before_following_bytes() {
    let context = StackCodecContext::v26_2();
    let bytes = [54, 1, 7, 0, 0, 0xa3, 2];
    let mut reader = Reader::new(&bytes);
    let (name, options) = read_particle(&mut reader, &context).unwrap();
    assert_eq!(name, "minecraft:item");
    assert_eq!(options, ParticleOptions::None);
    assert_eq!(reader.remaining_bytes(), &[0xa3, 2]);
}
