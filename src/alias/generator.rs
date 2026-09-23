use rand::Rng;
use rand::distributions::Alphanumeric;

const WORDS: &[&str] = &[
    "amber", "basalt", "cedar", "delta", "ember", "fjord", "gable", "harbor", "indigo", "juniper",
    "kelp", "lumen", "marble", "nimbus", "onyx", "pebble", "quartz", "ripple", "slate", "tundra",
    "umber", "vellum", "willow", "xenon", "yarrow", "zephyr",
];

/// Lowercase alphanumeric token, e.g. `9f2a1b`.
pub fn random_token(length: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(|byte| char::from(byte).to_ascii_lowercase())
        .collect()
}

/// Local part for a generated alias, e.g. `cedar-ripple.4f8a`.
pub fn random_local_part() -> String {
    let mut rng = rand::thread_rng();
    let first = WORDS[rng.gen_range(0..WORDS.len())];
    let second = WORDS[rng.gen_range(0..WORDS.len())];
    format!("{first}-{second}.{}", random_token(4))
}

#[cfg(test)]
mod tests {
    use super::{random_local_part, random_token};

    #[test]
    fn tokens_have_the_requested_length_and_are_lowercase() {
        let token = random_token(8);
        assert_eq!(token.len(), 8);
        assert_eq!(token, token.to_lowercase());
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn generated_local_parts_are_unique_and_address_safe() {
        let first = random_local_part();
        let second = random_local_part();
        assert_ne!(first, second);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        );
    }
}
