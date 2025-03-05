use std::env;

pub fn get_volume_up_key() -> u32 {
    let raw = env::var("volume_up_key").unwrap();
    let without_prefix = raw.trim_start_matches("0x");
    return u32::from_str_radix(without_prefix, 16).unwrap_or(0x82);
}

pub fn get_volume_down_key() -> u32 {
    let raw = env::var("volume_down_key").unwrap();
    let without_prefix = raw.trim_start_matches("0x");
    return u32::from_str_radix(without_prefix, 16).unwrap_or(0x81);
}

pub fn get_volume_increment() -> u8 {
    return env::var("VOLUME_INCREMENT").unwrap().parse().unwrap_or(5);
}
