//! The encryption of the server browser's replies: GameSpy's GOACrypt
//! (`P/src/RVL_SDK/gamespy/serverbrowsing/sb_crypt.c`) under a key the client
//! derives from its own challenge, the game's secret key and part of the
//! reply's header (`InitCryptKey` in `sb_serverlist.c`).

/// The client's challenge: 8 bytes of its list request.
pub const CHALLENGE_LEN: usize = 8;
/// The reply's header: 20 bytes, of which the client reads two lengths and
/// mixes the bytes after the first 7 into its key.
const HEADER_LEN: usize = 20;
const KEY_OFFSET: usize = 7;

/// Encrypts `reply` for a client of `challenge`: the header, then the reply
/// under GOACrypt. `random` fills the header's free bytes.
pub fn encrypt(
    secret: &[u8],
    challenge: &[u8; CHALLENGE_LEN],
    random: [u8; HEADER_LEN],
    reply: &[u8],
) -> Vec<u8> {
    let mut header = random;
    // The first byte gives where the key starts, the one before it how long
    // it is; bytes 1 and 2 are the backend's game flags, none.
    header[0] = (KEY_OFFSET as u8 - 2) ^ 0xEC;
    header[1] = 0;
    header[2] = 0;
    header[KEY_OFFSET - 1] = (HEADER_LEN - KEY_OFFSET) as u8 ^ 0xEA;

    let key = derive(secret, challenge, &header[KEY_OFFSET..]);
    let mut state = State::new(&key);
    let mut out = header.to_vec();
    out.extend(reply.iter().map(|&byte| state.encrypt(byte)));
    out
}

/// `InitCryptKey`: the client's challenge, mixed with the secret key and the
/// header's key bytes.
fn derive(secret: &[u8], challenge: &[u8; CHALLENGE_LEN], key: &[u8]) -> [u8; CHALLENGE_LEN] {
    let mut mixed = *challenge;
    for (i, &byte) in key.iter().enumerate() {
        let at = (i * usize::from(secret[i % secret.len()])) % CHALLENGE_LEN;
        mixed[at] ^= mixed[i % CHALLENGE_LEN] ^ byte;
    }
    mixed
}

/// `GOACryptState`: a permutation of the 256 bytes and five indices.
struct State {
    cards: [u8; 256],
    rotor: u8,
    ratchet: u8,
    avalanche: u8,
    last_plain: u8,
    last_cipher: u8,
}

impl State {
    /// `GOACryptInit`: the cards shuffled by the key.
    fn new(key: &[u8]) -> Self {
        let mut cards: [u8; 256] = std::array::from_fn(|i| i as u8);
        let (mut rsum, mut keypos) = (0u8, 0usize);
        for i in (0..256).rev() {
            let swap = usize::from(key_rand(&cards, i, key, &mut rsum, &mut keypos));
            cards.swap(i, swap);
        }
        Self {
            rotor: cards[1],
            ratchet: cards[3],
            avalanche: cards[5],
            last_plain: cards[7],
            last_cipher: cards[usize::from(rsum)],
            cards,
        }
    }

    /// `GOAEncryptByte`.
    fn encrypt(&mut self, plain: u8) -> u8 {
        self.shuffle();
        self.last_cipher = plain ^ self.mask();
        self.last_plain = plain;
        self.last_cipher
    }

    /// `GOADecryptByte`, for the tests: what the client does.
    #[cfg(test)]
    fn decrypt(&mut self, cipher: u8) -> u8 {
        self.shuffle();
        self.last_plain = cipher ^ self.mask();
        self.last_cipher = cipher;
        self.last_plain
    }

    fn shuffle(&mut self) {
        let card = |index: u8| usize::from(index);
        self.ratchet = self.ratchet.wrapping_add(self.cards[card(self.rotor)]);
        self.rotor = self.rotor.wrapping_add(1);
        let swap = self.cards[card(self.last_cipher)];
        self.cards[card(self.last_cipher)] = self.cards[card(self.ratchet)];
        self.cards[card(self.ratchet)] = self.cards[card(self.last_plain)];
        self.cards[card(self.last_plain)] = self.cards[card(self.rotor)];
        self.cards[card(self.rotor)] = swap;
        self.avalanche = self.avalanche.wrapping_add(self.cards[card(swap)]);
    }

    fn mask(&self) -> u8 {
        let card = |index: u8| self.cards[usize::from(index)];
        card(card(self.avalanche).wrapping_add(card(self.rotor)))
            ^ card(card(
                card(self.last_plain)
                    .wrapping_add(card(self.last_cipher))
                    .wrapping_add(card(self.ratchet)),
            ))
    }
}

/// `keyrand`: a card from 0 to `limit`, drawn with the key.
fn key_rand(cards: &[u8; 256], limit: usize, key: &[u8], rsum: &mut u8, keypos: &mut usize) -> u8 {
    if limit == 0 {
        return 0;
    }
    let mut mask = 1usize;
    while mask < limit {
        mask = (mask << 1) + 1;
    }
    let mut retries = 0;
    loop {
        *rsum = cards[usize::from(*rsum)].wrapping_add(key[*keypos]);
        *keypos += 1;
        if *keypos >= key.len() {
            *keypos = 0;
            *rsum = rsum.wrapping_add(key.len() as u8);
        }
        let mut value = mask & usize::from(*rsum);
        retries += 1;
        if retries > 11 {
            value %= limit;
        }
        if value <= limit {
            return value as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"B4LdGW";
    /// The port's challenge in its first list request.
    const CHALLENGE: &[u8; 8] = b"Y];98gol";
    const RANDOM: [u8; 20] = [
        0, 0, 0, 0x11, 0x22, 0x33, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13,
    ];

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn replies_encrypt_as_gamespys_own_code() {
        // Encrypted by sb_crypt.c's GOAEncrypt under InitCryptKey's key.
        let plain = hex(
            "ac110001922a010d77635f706964000054ac110001aeaf000000ff313030303030303030300000ffffffff",
        );
        let cipher = hex(
            "dd88f2486d31b247d869420ff684501a11449f94b1116dd7eda7f11da967fbe97c7c861eec6ea7634e9e22",
        );
        let out = encrypt(SECRET, CHALLENGE, RANDOM, &plain);
        assert_eq!(out[..7], [0xE9, 0, 0, 0x11, 0x22, 0x33, 0xE7]);
        assert_eq!(out[7..20], RANDOM[7..]);
        assert_eq!(out[20..], cipher);
    }

    #[test]
    fn the_client_decrypts_them() {
        let plain = b"\xAC\x11\x00\x01any reply at all";
        let out = encrypt(SECRET, CHALLENGE, rand::random(), plain);
        // ProcessMainListData: the key's place from the header, then decrypt.
        let key_at = usize::from(out[0] ^ 0xEC) + 2;
        let key_len = usize::from(out[key_at - 1] ^ 0xEA);
        let mut state = State::new(&derive(SECRET, CHALLENGE, &out[key_at..key_at + key_len]));
        let decrypted: Vec<u8> = out[key_at + key_len..]
            .iter()
            .map(|&byte| state.decrypt(byte))
            .collect();
        assert_eq!(decrypted, plain);
    }
}
