//! Secret redaction.
//!
//! Redaction happens inside the collectors, before a value ever reaches a
//! snapshot. A value is treated as secret when its *name* looks secret
//! (`STRIPE_SECRET_KEY`), or when the *value* does (a URL with a password, a
//! known token prefix such as `ghp_`, a JWT, a PEM block, or a long
//! mixed-alphabet string).
//!
//! Redacted values keep a fingerprint: a truncated HMAC-SHA256 of the original
//! under a per-installation key. The diff can then say "this secret changed"
//! without the snapshot revealing it, and without the fingerprint being
//! brute-forceable by someone who only has the snapshot file.

use hostprint_model::REDACTED;
use sha2::{Digest, Sha256};

/// Name fragments that mark a variable or flag as secret (case-insensitive).
pub const DEFAULT_SECRET_WORDS: &[&str] = &[
    "PASSWORD",
    "PASSWD",
    "PASS",
    "SECRET",
    "TOKEN",
    "KEY",
    "AUTH",
    "COOKIE",
    "PRIVATE",
    "CREDENTIAL",
    "SESSION",
    "SIGNATURE",
    "SALT",
    "DSN",
];

/// Prefixes of well-known credential formats.
const TOKEN_PREFIXES: &[&str] = &[
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "rk_test_",
    "whsec_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
    "xapp-",
    "AKIA",
    "ASIA",
    "AIza",
    "sk-ant-",
    "sk-proj-",
    "npm_",
    "pypi-",
    "hf_",
    "SG.",
    "shpat_",
    "shpss_",
    "dop_v1_",
    "-----BEGIN",
];

/// Length of stored fingerprints, in hex characters.
const FINGERPRINT_LEN: usize = 16;

#[derive(Clone)]
pub struct Redactor {
    key: Vec<u8>,
    key_id: String,
    enabled: bool,
    words: Vec<String>,
    allow: Vec<String>,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor")
            .field("key_id", &self.key_id)
            .field("enabled", &self.enabled)
            .field("words", &self.words)
            .field("allow", &self.allow)
            .finish_non_exhaustive()
    }
}

/// A value prepared for storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    pub value: String,
    pub redacted: bool,
    pub fingerprint: Option<String>,
}

impl Redactor {
    pub fn new(key: &[u8]) -> Self {
        let mut id_input = b"hostprint-fingerprint-key:".to_vec();
        id_input.extend_from_slice(key);
        Redactor {
            key: key.to_vec(),
            key_id: hex(&Sha256::digest(&id_input))[..12].to_string(),
            enabled: true,
            words: DEFAULT_SECRET_WORDS.iter().map(|w| w.to_string()).collect(),
            allow: Vec::new(),
        }
    }

    /// Disables redaction entirely (values are stored verbatim).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Adds name fragments that mark a value as secret.
    pub fn with_words<I: IntoIterator<Item = String>>(mut self, words: I) -> Self {
        self.words.extend(words.into_iter().map(|w| w.to_ascii_uppercase()));
        self
    }

    /// Names that are never redacted by name (exact, case-insensitive).
    pub fn with_allowed<I: IntoIterator<Item = String>>(mut self, names: I) -> Self {
        self.allow.extend(names.into_iter().map(|w| w.to_ascii_uppercase()));
        self
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Keyed fingerprint of a value.
    pub fn fingerprint(&self, value: &str) -> String {
        hex(&hmac_sha256(&self.key, value.as_bytes()))[..FINGERPRINT_LEN].to_string()
    }

    pub fn is_secret_name(&self, name: &str) -> bool {
        let upper = name.to_ascii_uppercase();
        if self.allow.contains(&upper) {
            return false;
        }
        self.words.iter().any(|w| upper.contains(w.as_str()))
    }

    /// Prepares a `name=value` pair (environment variable, flag) for storage.
    pub fn pair(&self, name: &str, value: &str) -> Redacted {
        if !self.enabled || value.is_empty() {
            return self.plain(value);
        }
        if self.is_secret_name(name) {
            return self.hidden(REDACTED.to_string(), value);
        }
        match self.scrub_value(value) {
            Some(scrubbed) => self.hidden(scrubbed, value),
            None => self.plain(value),
        }
    }

    /// Redacts a free-standing value: returns the value with any credentials
    /// removed, or unchanged if nothing looks secret.
    pub fn value(&self, value: &str) -> String {
        if !self.enabled {
            return value.to_string();
        }
        self.scrub_value(value).unwrap_or_else(|| value.to_string())
    }

    /// Redacts credentials inside free text such as a log line, keeping
    /// everything else, including spacing and punctuation, as it was.
    ///
    /// Handles `key=value` and `"key":"value"` pairs with secret-looking keys,
    /// a bare secret key followed by its value (`password: hunter2`,
    /// `Authorization: Bearer <token>`), URLs with credentials, and
    /// token-shaped words.
    pub fn text(&self, line: &str) -> String {
        if !self.enabled {
            return line.to_string();
        }
        let is_separator = |c: char| c.is_whitespace() || ",{}&;()[]".contains(c);
        let mut out = String::with_capacity(line.len());
        let mut pending = 0u8; // following words that are secret values
        let mut rest = line;
        while !rest.is_empty() {
            let sep_len = rest.find(|c: char| !is_separator(c)).unwrap_or(rest.len());
            out.push_str(&rest[..sep_len]);
            rest = &rest[sep_len..];
            if rest.is_empty() {
                break;
            }
            let word_len = rest.find(is_separator).unwrap_or(rest.len());
            let word = &rest[..word_len];
            rest = &rest[word_len..];

            if pending > 0 {
                pending -= 1;
                // "Authorization: Bearer <token>": keep the scheme, redact the token.
                let scheme = matches!(word.to_ascii_lowercase().as_str(), "bearer" | "basic" | "token" | "digest");
                if scheme && pending == 0 {
                    out.push_str(word);
                    pending = 1;
                } else {
                    out.push_str(REDACTED);
                }
                continue;
            }
            let (scrubbed, value_follows) = self.scrub_word(word);
            out.push_str(&scrubbed);
            if value_follows {
                pending = 1;
            }
        }
        out
    }

    /// Redacts one word of free text. Also returns whether the word is a bare
    /// secret key whose value is the next word.
    fn scrub_word(&self, word: &str) -> (String, bool) {
        if let Some(pos) = word.find(['=', ':']) {
            let (key, rest) = word.split_at(pos);
            let (separator, value) = rest.split_at(1);
            let name = key.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'));
            // `scheme://` is a URL, not a key; URLs are handled below.
            if !name.is_empty() && !value.starts_with("//") && self.is_secret_name(name) {
                let lead_len = value.len() - value.trim_start_matches(['"', '\'']).len();
                let core = value[lead_len..].trim_end_matches(['"', '\'', '.', ':']);
                if core.is_empty() {
                    return (word.to_string(), true);
                }
                let trail = &value[lead_len + core.len()..];
                return (format!("{key}{separator}{}{REDACTED}{trail}", &value[..lead_len]), false);
            }
        }
        if let Some(flag) = word.strip_prefix('-') {
            if self.is_secret_name(flag.trim_start_matches('-')) {
                return (word.to_string(), true);
            }
        }
        (self.value(word), false)
    }

    /// Redacts secret-looking arguments in a command line.
    pub fn args(&self, args: &[String]) -> Vec<String> {
        if !self.enabled {
            return args.to_vec();
        }
        let mut out = Vec::with_capacity(args.len());
        let mut redact_next = false;
        for arg in args {
            if redact_next {
                redact_next = false;
                if !arg.starts_with('-') {
                    out.push(REDACTED.to_string());
                    continue;
                }
            }
            if let Some(flag) = arg.strip_prefix('-') {
                let flag = flag.trim_start_matches('-');
                if let Some((name, value)) = flag.split_once('=') {
                    if self.is_secret_name(name) && !value.is_empty() {
                        let prefix_len = arg.len() - flag.len();
                        out.push(format!("{}{}={}", &arg[..prefix_len], name, REDACTED));
                        continue;
                    }
                } else if self.is_secret_name(flag) {
                    redact_next = true;
                }
            } else if let Some((name, value)) = arg.split_once('=') {
                if is_identifier(name) && self.is_secret_name(name) && !value.is_empty() {
                    out.push(format!("{name}={REDACTED}"));
                    continue;
                }
            }
            // An argument can itself be a script (`sh -c '... api_key=...'`),
            // so it gets the same treatment as free text.
            out.push(self.text(arg));
        }
        out
    }

    fn plain(&self, value: &str) -> Redacted {
        Redacted { value: value.to_string(), redacted: false, fingerprint: None }
    }

    fn hidden(&self, stored: String, original: &str) -> Redacted {
        Redacted { value: stored, redacted: true, fingerprint: Some(self.fingerprint(original)) }
    }

    /// `Some(scrubbed)` if the value contains a credential.
    fn scrub_value(&self, value: &str) -> Option<String> {
        if value.contains("://") {
            let scrubbed = self.scrub_url(value);
            if scrubbed != value {
                return Some(scrubbed);
            }
        }
        looks_like_token(value).then(|| REDACTED.to_string())
    }

    /// Removes passwords (and token-like usernames) from a URL's userinfo, and
    /// values of secret-named query parameters.
    fn scrub_url(&self, url: &str) -> String {
        let Some(scheme_end) = url.find("://") else {
            return url.to_string();
        };
        let authority_start = scheme_end + 3;
        let rest = &url[authority_start..];
        let authority_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_len];
        let mut out = url[..authority_start].to_string();

        match authority.rfind('@') {
            Some(at) => {
                let userinfo = &authority[..at];
                match userinfo.split_once(':') {
                    Some((user, pass)) if !pass.is_empty() => {
                        out.push_str(user);
                        out.push(':');
                        out.push_str(REDACTED);
                    }
                    _ if looks_like_token(userinfo) => out.push_str(REDACTED),
                    _ => out.push_str(userinfo),
                }
                out.push_str(&authority[at..]);
            }
            None => out.push_str(authority),
        }

        let tail = &rest[authority_len..];
        let (path, query) = match tail.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (tail, None),
        };
        out.push_str(path);
        if let Some(query) = query {
            let (query, fragment) = match query.split_once('#') {
                Some((q, f)) => (q, Some(f)),
                None => (query, None),
            };
            out.push('?');
            let params: Vec<String> = query
                .split('&')
                .map(|param| match param.split_once('=') {
                    Some((name, value)) if !value.is_empty() && self.is_secret_name(name) => {
                        format!("{name}={REDACTED}")
                    }
                    _ => param.to_string(),
                })
                .collect();
            out.push_str(&params.join("&"));
            if let Some(fragment) = fragment {
                out.push('#');
                out.push_str(fragment);
            }
        }
        out
    }
}

/// Heuristic for credential-shaped values.
pub fn looks_like_token(value: &str) -> bool {
    let v = value.trim();
    if TOKEN_PREFIXES.iter().any(|p| v.starts_with(p) && v.len() >= p.len() + 8) {
        return true;
    }
    // JWT: three base64url segments, the header starting with `{"`.
    if v.starts_with("eyJ") && v.len() > 30 && v.split('.').count() == 3 {
        return true;
    }
    // Long random-looking strings. Requiring all three character classes
    // spares hex digests, UUIDs, words and paths.
    v.len() >= 32
        && v.chars().all(|c| c.is_ascii_alphanumeric() || "+=_-.".contains(c))
        && v.chars().any(|c| c.is_ascii_lowercase())
        && v.chars().any(|c| c.is_ascii_uppercase())
        && v.chars().any(|c| c.is_ascii_digit())
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block_key = [0u8; BLOCK];
    if key.len() > BLOCK {
        block_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= block_key[i];
        opad[i] ^= block_key[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(message).finalize();
    Sha256::new().chain_update(opad).chain_update(inner).finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        Redactor::new(b"test-key")
    }

    #[test]
    fn hmac_matches_rfc4231_test_case_2() {
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn redacts_by_name() {
        let r = redactor();
        for name in ["JWT_SECRET", "API_KEY", "STRIPE_SECRET_KEY", "db_password", "GITHUB_TOKEN"] {
            let out = r.pair(name, "hunter2");
            assert!(out.redacted, "{name} should be redacted");
            assert_eq!(out.value, REDACTED);
            assert_eq!(out.fingerprint.as_deref().map(str::len), Some(FINGERPRINT_LEN));
        }
        let plain = r.pair("DATABASE_POOL_SIZE", "20");
        assert_eq!(plain, Redacted { value: "20".into(), redacted: false, fingerprint: None });
    }

    #[test]
    fn empty_secrets_are_kept_visible() {
        let out = redactor().pair("JWT_SECRET", "");
        assert!(!out.redacted);
        assert_eq!(out.value, "");
    }

    #[test]
    fn fingerprints_track_changes_without_revealing_values() {
        let r = redactor();
        let a = r.pair("API_KEY", "one").fingerprint.unwrap();
        let b = r.pair("API_KEY", "one").fingerprint.unwrap();
        let c = r.pair("API_KEY", "two").fingerprint.unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        // A different installation key gives unrelated fingerprints.
        let other = Redactor::new(b"other-key").pair("API_KEY", "one").fingerprint.unwrap();
        assert_ne!(a, other);
        assert_ne!(redactor().key_id(), Redactor::new(b"other-key").key_id());
    }

    #[test]
    fn redacts_url_credentials() {
        let r = redactor();
        let out = r.pair("DATABASE_URL", "postgres://app:s3cret@db.internal:5432/app");
        assert!(out.redacted);
        assert_eq!(out.value, "postgres://app:[REDACTED]@db.internal:5432/app");

        assert_eq!(
            r.value("https://ghp_abcdefghijklmnop1234@github.com/org/repo.git"),
            "https://[REDACTED]@github.com/org/repo.git"
        );
        assert_eq!(r.value("https://git@github.com/org/repo.git"), "https://git@github.com/org/repo.git");
        assert_eq!(
            r.value("https://api.example.com/v1?user=bob&api_key=abc123#top"),
            "https://api.example.com/v1?user=bob&api_key=[REDACTED]#top"
        );
        let plain = r.pair("REDIS_URL", "redis://cache:6379/0");
        assert!(!plain.redacted);
    }

    #[test]
    fn redacts_token_shaped_values() {
        let r = redactor();
        assert!(r.pair("SOME_VAR", "sk_live_51HxYzAbCdEfGhIjKlMn").redacted);
        assert!(r.pair("WEBHOOK", "xoxb-1234-5678-abcdefghij").redacted);
        assert!(r.pair("X", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJlc2ln").redacted);
        assert!(r.pair("X", "Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MEFCQ0RFRkdI").redacted);
        // Not secrets: hex digests, UUIDs, paths, words.
        assert!(!r.pair("COMMIT", "e821aa4c9d1b2f3e4a5b6c7d8e9f0a1b2c3d4e5f").redacted);
        assert!(!r.pair("ID", "550e8400-e29b-41d4-a716-446655440000").redacted);
        assert!(!r.pair("PATH", "/usr/local/bin:/usr/bin:/bin").redacted);
        assert!(!r.pair("GREETING", "Hello World").redacted);
    }

    #[test]
    fn redacts_command_line_arguments() {
        let r = redactor();
        let args: Vec<String> = [
            "redis-server",
            "--requirepass",
            "hunter2",
            "--port",
            "6379",
            "--api-key=abc",
            "PGPASSWORD=xyz",
            "-p",
            "8080",
            "https://u:p@host/x",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            r.args(&args),
            [
                "redis-server",
                "--requirepass",
                REDACTED,
                "--port",
                "6379",
                "--api-key=[REDACTED]",
                "PGPASSWORD=[REDACTED]",
                "-p",
                "8080",
                "https://u:[REDACTED]@host/x",
            ]
        );
        // A secret-named flag followed by another flag redacts nothing.
        let args: Vec<String> = ["app", "--no-auth", "--verbose"].iter().map(|s| s.to_string()).collect();
        assert_eq!(r.args(&args), args);
        // Secrets inside a script argument (found by a real capture of
        // `sh -c '... api_key=...'`).
        let args: Vec<String> = ["sh", "-c", "echo charge failed api_key=sk_live_abcdefghijklmnop; sleep 2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(r.args(&args)[2], "echo charge failed api_key=[REDACTED]; sleep 2");
    }

    #[test]
    fn redacts_free_text() {
        let r = redactor();
        let cases = [
            ("connecting with password=hunter2 to db", "connecting with password=[REDACTED] to db"),
            (
                r#"{"user":"bob","password":"hunter2","port":5432}"#,
                r#"{"user":"bob","password":"[REDACTED]","port":5432}"#,
            ),
            ("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig", "Authorization: Bearer [REDACTED]"),
            ("login failed, password: hunter2 (attempt 3)", "login failed, password: [REDACTED] (attempt 3)"),
            ("dial redis://:s3cret@redis:6379 refused", "dial redis://:[REDACTED]@redis:6379 refused"),
            ("GET /hook?id=7&token=abc123 200", "GET /hook?id=7&token=[REDACTED] 200"),
            ("using key ghp_abcdefghijklmnop1234 now", "using key [REDACTED] now"),
            ("started --api-key s3cret --port 80", "started --api-key [REDACTED] --port 80"),
        ];
        for (input, expected) in cases {
            assert_eq!(r.text(input), expected, "{input}");
        }
        // Ordinary log lines are untouched, whitespace included.
        let plain = "2026-10-01T14:30:01Z  INFO  Ready to accept connections tcp:6379";
        assert_eq!(r.text(plain), plain);
    }

    #[test]
    fn allow_list_and_disable() {
        let r = redactor().with_allowed(["SSH_AUTH_SOCK".to_string()]);
        assert!(!r.pair("SSH_AUTH_SOCK", "/tmp/ssh-XXXX/agent.1").redacted);
        let r = redactor().with_words(["internal".to_string()]);
        assert!(r.pair("INTERNAL_URL_HOST", "10.0.0.1").redacted);
        let r = redactor().enabled(false);
        assert_eq!(r.pair("API_KEY", "abc").value, "abc");
    }
}
