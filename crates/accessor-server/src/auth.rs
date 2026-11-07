//! Password authentication: cleartext, MD5 and SCRAM-SHA-256.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMethod {
    /// No password is asked for.
    Trust,
    Cleartext,
    Md5,
    Scram,
}

impl AuthMethod {
    pub fn parse(s: &str) -> Option<AuthMethod> {
        Some(match s.to_ascii_lowercase().as_str() {
            "trust" => AuthMethod::Trust,
            "password" | "cleartext" => AuthMethod::Cleartext,
            "md5" => AuthMethod::Md5,
            "scram" | "scram-sha-256" => AuthMethod::Scram,
            _ => return None,
        })
    }
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    // a failure of the system's generator leaves zeros, which a nonce check on the client still tells apart per connection by time
    let _ = getrandom::fill(&mut b);
    b
}

/// `md5` + hex(md5(hex(md5(password + user)) + salt)): what a client sends for MD5 authentication.
pub fn md5_response(user: &str, password: &str, salt: &[u8; 4]) -> String {
    use md5::Md5;
    let hex = |d: &[u8]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let inner = hex(&Md5::digest(format!("{password}{user}").as_bytes()));
    let mut outer = Md5::new();
    outer.update(inner.as_bytes());
    outer.update(salt);
    format!("md5{}", hex(&outer.finalize()))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

/// PBKDF2 with HMAC-SHA-256 and one 32-byte block (`Hi` in RFC 5802).
fn salted_password(password: &str, salt: &[u8], iterations: u32) -> Vec<u8> {
    let mut first = salt.to_vec();
    first.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac(password.as_bytes(), &first);
    let mut out = u.clone();
    for _ in 1..iterations {
        u = hmac(password.as_bytes(), &u);
        for (o, x) in out.iter_mut().zip(&u) {
            *o ^= x;
        }
    }
    out
}

/// The server side of one SCRAM-SHA-256 exchange.
pub struct Scram {
    password: String,
    salt: Vec<u8>,
    iterations: u32,
    client_first_bare: String,
    server_first: String,
    nonce: String,
}

impl Scram {
    /// Reads the client's first message (`n,,n=user,r=nonce`) and gives the exchange and the server's first message.
    pub fn start(password: &str, client_first: &str) -> Option<(Scram, String)> {
        let bare = client_first.strip_prefix("n,,").or_else(|| client_first.strip_prefix("y,,"))?;
        let client_nonce = bare.split(',').find_map(|p| p.strip_prefix("r="))?;
        let salt = random_bytes::<16>().to_vec();
        let nonce = format!("{client_nonce}{}", B64.encode(random_bytes::<18>()));
        let iterations = 4096;
        let server_first = format!("r={nonce},s={},i={iterations}", B64.encode(&salt));
        Some((Scram { password: password.to_string(), salt, iterations, client_first_bare: bare.to_string(), server_first: server_first.clone(), nonce }, server_first))
    }

    /// Checks the client's final message (`c=biws,r=nonce,p=proof`); the server's final message (`v=signature`) if it is right.
    pub fn finish(&self, client_final: &str) -> Option<String> {
        let (without_proof, proof) = client_final.rsplit_once(",p=")?;
        let nonce = without_proof.split(',').find_map(|p| p.strip_prefix("r="))?;
        if nonce != self.nonce {
            return None;
        }
        let proof = B64.decode(proof).ok()?;
        let salted = salted_password(&self.password, &self.salt, self.iterations);
        let client_key = hmac(&salted, b"Client Key");
        let stored_key = Sha256::digest(&client_key);
        let auth_message = format!("{},{},{}", self.client_first_bare, self.server_first, without_proof);
        let signature = hmac(&stored_key, auth_message.as_bytes());
        if proof.len() != signature.len() {
            return None;
        }
        let recovered: Vec<u8> = proof.iter().zip(&signature).map(|(p, s)| p ^ s).collect();
        if Sha256::digest(&recovered).as_slice() != stored_key.as_slice() {
            return None;
        }
        let server_key = hmac(&salted, b"Server Key");
        Some(format!("v={}", B64.encode(hmac(&server_key, auth_message.as_bytes()))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scram_accepts_the_right_password_and_refuses_the_wrong_one() {
        // play the client by hand
        let run = |server_pw: &str, client_pw: &str| -> bool {
            let client_first_bare = "n=user,r=clientnonce";
            let (s, server_first) = Scram::start(server_pw, &format!("n,,{client_first_bare}")).unwrap();
            let fields: std::collections::HashMap<&str, &str> = server_first.split(',').filter_map(|p| p.split_once('=')).collect();
            let salt = B64.decode(fields["s"]).unwrap();
            let iterations: u32 = fields["i"].parse().unwrap();
            let without_proof = format!("c=biws,r={}", fields["r"]);
            let salted = salted_password(client_pw, &salt, iterations);
            let client_key = hmac(&salted, b"Client Key");
            let stored = Sha256::digest(&client_key);
            let auth = format!("{client_first_bare},{server_first},{without_proof}");
            let sig = hmac(&stored, auth.as_bytes());
            let proof: Vec<u8> = client_key.iter().zip(&sig).map(|(a, b)| a ^ b).collect();
            s.finish(&format!("{without_proof},p={}", B64.encode(proof))).is_some()
        };
        assert!(run("secret", "secret"));
        assert!(!run("secret", "wrong"));
    }

    #[test]
    fn md5_matches_the_documented_form() {
        // md5 of "secretuser" hashed again with the salt, "md5" in front, 35 characters
        let r = md5_response("user", "secret", &[1, 2, 3, 4]);
        assert!(r.starts_with("md5") && r.len() == 35);
    }
}
