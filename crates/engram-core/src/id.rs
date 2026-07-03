//! Short, sortable, URL-safe ids, ported from yapp's `IdGenerator`: 12 chars
//! of lowercase base36 (`a-z0-9`). The first 7 chars encode seconds since
//! 2026-01-01 UTC (time-sortable, room for ~2500 years); the last 5 are
//! random. Replaces UUIDs at every mint site — a third of the length, so ids
//! stay cheap in AI context windows. Existing UUID rows remain valid (ids are
//! opaque TEXT).

/// Unix seconds at 2026-01-01T00:00:00Z.
const EPOCH_SECONDS: i64 = 1_767_225_600;

const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const TIMESTAMP_CHARS: usize = 7;
const RANDOM_CHARS: usize = 5;
pub const ID_LENGTH: usize = TIMESTAMP_CHARS + RANDOM_CHARS;

pub fn new_id() -> String {
    let mut buf = [0u8; ID_LENGTH];

    let mut seconds = (crate::now() - EPOCH_SECONDS).max(0) as u64;
    for slot in buf[..TIMESTAMP_CHARS].iter_mut().rev() {
        *slot = ALPHABET[(seconds % 36) as usize];
        seconds /= 36;
    }

    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("OS RNG");
    let mut rnd = u64::from_le_bytes(bytes);
    for slot in buf[TIMESTAMP_CHARS..].iter_mut() {
        *slot = ALPHABET[(rnd % 36) as usize];
        rnd /= 36;
    }

    String::from_utf8(buf.to_vec()).expect("base36 is ascii")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_short_base36_and_time_sortable() {
        let a = new_id();
        assert_eq!(a.len(), ID_LENGTH);
        assert!(a.bytes().all(|b| ALPHABET.contains(&b)));

        // same second → same prefix; the timestamp half is lexicographically sortable
        let b = new_id();
        assert_eq!(a[..TIMESTAMP_CHARS - 1], b[..TIMESTAMP_CHARS - 1]);
        assert_ne!(a, b, "random tail differentiates ids minted together");
    }
}
