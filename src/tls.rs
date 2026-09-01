/*
 * SonarScanner for Cargo
 * Copyright (C) SonarSource Sàrl
 * mailto:info AT sonarsource DOT com
 *
 * You can redistribute and/or modify this program under the terms of
 * the Sonar Source-Available License Version 1, as published by SonarSource Sàrl.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.
 * See the Sonar Source-Available License for more details.
 *
 * You should have received a copy of the Sonar Source-Available License
 * along with this program; if not, see https://sonarsource.com/license/ssal/
 */
//! The PKCS#12 truststore and keystore the bootstrapper uses for its own HTTPS calls.
//!
//! Both are part of the bootstrapping contract, which marks all four properties as ones the
//! bootstrapper *uses* rather than merely forwards, and gives each a default path under
//! `<sonar.userHome>/ssl`. The scanner engine is configured by the same properties and reads the
//! same files for its own calls; nothing here stops them being passed on.
//!
//! Two consequences of the contract are worth stating, because they are not obvious:
//!
//! * A truststore is **additive** — "used by the Scanner in addition to OS + built-in
//!   certificates". Trust is widened, never replaced, so a store holding only a corporate root does
//!   not stop the scanner reaching SonarQube Cloud.
//! * The defaults are probed **unconditionally**. A user who drops a `truststore.p12` into
//!   `~/.sonar/ssl` has configured the scanner, without setting any property.

use std::path::{Path, PathBuf};

use log::{debug, warn};
use p12_keystore::{KeyStore, KeyStoreEntry, Pkcs12ImportPolicy};
use thiserror::Error;

use crate::config::Properties;

pub const TRUSTSTORE_PATH: &str = "sonar.scanner.truststorePath";
pub const TRUSTSTORE_PASSWORD: &str = "sonar.scanner.truststorePassword";
pub const KEYSTORE_PATH: &str = "sonar.scanner.keystorePath";
pub const KEYSTORE_PASSWORD: &str = "sonar.scanner.keystorePassword";

/// Tried in order when no password is configured, as the contract specifies: `changeit` is the Java
/// keytool default, and `sonar` is the one the Sonar tooling has historically written.
const DEFAULT_PASSWORDS: [&str; 2] = ["changeit", "sonar"];

/// Directory below `sonar.userHome` holding both default stores.
const SSL_DIRECTORY: &str = "ssl";
const DEFAULT_TRUSTSTORE: &str = "truststore.p12";
const DEFAULT_KEYSTORE: &str = "keystore.p12";

#[derive(Debug, Error)]
pub enum TlsError {
    #[error("Failed to read {kind} {path}: {source}")]
    Read {
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "The {kind} {path} could not be opened. Check {password_property}; the password is \
         needed even to read the certificates it holds."
    )]
    Password { kind: &'static str, path: PathBuf, password_property: &'static str },

    #[error("The {kind} {path} is not a readable PKCS#12 file: {message}")]
    Malformed { kind: &'static str, path: PathBuf, message: String },

    #[error(
        "The keystore {path} holds no private key, so there is no client certificate to present. \
         A keystore needs a key and its certificate chain; a file of certificates alone is a \
         truststore, and belongs in {truststore_property}."
    )]
    NoPrivateKey { path: PathBuf, truststore_property: &'static str },

    #[error("The truststore {path} holds no certificates.")]
    NoCertificates { path: PathBuf },

    #[error(
        "The keystore {path} holds a private key \"{alias}\" with no certificate, so there is \
         nothing to present to the server. Export the key together with its certificate chain."
    )]
    KeyWithoutCertificate { path: PathBuf, alias: String },
}

type Result<T> = std::result::Result<T, TlsError>;

/// What the HTTP client should be configured with. Both fields are `None` when nothing is
/// configured and no default store exists, which is the ordinary case.
#[derive(Debug, Default)]
pub struct Stores {
    /// Every root to trust, DER-encoded: the platform's own plus the truststore's.
    ///
    /// `None` means "no truststore", and the caller should leave the platform verifier in place
    /// rather than pass a list. That distinction matters — see `resolve`.
    pub roots: Option<Vec<Vec<u8>>>,
    /// The client certificate chain and its private key, DER-encoded, the key as PKCS#8.
    pub client_identity: Option<Identity>,
}

#[derive(Debug)]
pub struct Identity {
    pub chain: Vec<Vec<u8>>,
    pub key: Vec<u8>,
}

/// Where a store came from, which decides what a missing file means.
enum Source {
    /// The user named this path, so it not existing is a mistake worth reporting.
    Configured(PathBuf),
    /// The contract's default location, which is probed and quietly skipped when absent.
    Default(PathBuf),
}

impl Source {
    fn path(&self) -> &Path {
        match self {
            Source::Configured(path) | Source::Default(path) => path,
        }
    }
}

/// Resolve both stores from the properties.
///
/// The truststore is returned as a *complete* root list — the platform's roots plus its own —
/// because the client cannot express "the platform verifier, and also these". Asking for specific
/// roots turns the built-in ones off, so they have to be enumerated and handed back. That is a real
/// loss on macOS and Windows, where the platform verifier applies per-certificate trust settings
/// that a flat list cannot carry, and it is why the substitution happens **only** when a truststore
/// actually exists: with no truststore, `roots` is `None` and the platform verifier is untouched.
pub fn resolve(properties: &Properties, user_home: &Path) -> Result<Stores> {
    let roots = match locate(properties, TRUSTSTORE_PATH, user_home, DEFAULT_TRUSTSTORE) {
        Some(source) => Some(load_roots(&source, properties)?),
        None => None,
    };
    let client_identity = match locate(properties, KEYSTORE_PATH, user_home, DEFAULT_KEYSTORE) {
        Some(source) => Some(load_identity(&source, properties)?),
        None => None,
    };
    Ok(Stores { roots, client_identity })
}

/// The store to load, or `None` when there is nothing to load.
fn locate(properties: &Properties, path_property: &str, user_home: &Path, default_name: &str) -> Option<Source> {
    if let Some(configured) = properties.get_non_blank(path_property) {
        return Some(Source::Configured(PathBuf::from(configured)));
    }
    let default = user_home.join(SSL_DIRECTORY).join(default_name);
    // Absent by default on most machines, so this is the quiet path, not an error.
    default.is_file().then_some(Source::Default(default))
}

fn load_roots(source: &Source, properties: &Properties) -> Result<Vec<Vec<u8>>> {
    // `Raw`, not `Relaxed`. `Relaxed` keeps a standalone certificate bag only when it carries Java's
    // Oracle trusted-key-usage attribute, which only `keytool` writes — so a truststore made the way
    // anyone without a JDK makes one, `openssl pkcs12 -export -nokeys`, parses into nothing at all.
    // Linking keys to certificates is what the other policies buy, and a truststore has no keys.
    let store = open(source, "truststore", properties, TRUSTSTORE_PASSWORD, Pkcs12ImportPolicy::Raw)?;
    let path = source.path();

    let mut certificates: Vec<Vec<u8>> = Vec::new();
    for (_, entry) in store.entries() {
        match entry {
            KeyStoreEntry::Certificate(certificate) => certificates.push(certificate.as_der().to_vec()),
            // A truststore holding a key as well is unusual but harmless: the chain is exactly the
            // set of certificates it vouches for, which is what a truststore is for.
            KeyStoreEntry::PrivateKeyChain(chain) => {
                certificates.extend(chain.certs().iter().map(|c| c.as_der().to_vec()));
            }
            KeyStoreEntry::Secret(_) => {}
        }
    }
    if certificates.is_empty() {
        return Err(TlsError::NoCertificates { path: path.to_path_buf() });
    }

    // The platform's roots have to be listed explicitly, because asking for specific roots turns
    // the built-in ones off. Without this the truststore would replace the OS trust store rather
    // than extend it, and the contract says extend.
    let platform = rustls_native_certs::load_native_certs();
    if platform.certs.is_empty() {
        // A minimal container with no `ca-certificates` and a mounted corporate truststore is the
        // case where that store is the *only* trust available. Refusing to start would be the one
        // outcome worse than not widening anything.
        let reason = platform.errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; ");
        warn!(
            "No operating-system certificates could be read ({}), so only the {} certificate(s) in {} \
             are trusted.",
            if reason.is_empty() { "none were found".to_string() } else { reason },
            certificates.len(),
            path.display()
        );
        return Ok(certificates);
    }
    // Not fatal on its own: some roots may be unreadable while the rest load fine.
    for error in &platform.errors {
        debug!("Ignoring an unreadable operating-system certificate: {error}");
    }

    let os_count = platform.certs.len();
    let mut roots: Vec<Vec<u8>> = platform.certs.into_iter().map(|c| c.as_ref().to_vec()).collect();
    debug!(
        "Trusting {} certificate(s) from {} in addition to {os_count} from the operating system",
        certificates.len(),
        path.display()
    );
    roots.extend(certificates);
    Ok(roots)
}

fn load_identity(source: &Source, properties: &Properties) -> Result<Identity> {
    // `Relaxed` here, unlike the truststore: this is exactly the case that needs a key matched to
    // its certificates.
    let store = open(source, "keystore", properties, KEYSTORE_PASSWORD, Pkcs12ImportPolicy::Relaxed)?;
    let path = source.path();

    let (alias, chain) = store
        .private_key_chain()
        .ok_or_else(|| TlsError::NoPrivateKey { path: path.to_path_buf(), truststore_property: TRUSTSTORE_PATH })?;
    // A key whose `localKeyID` matches no certificate still arrives as a chain, an empty one. Left
    // alone it would fail later, inside the TLS library, as an unusable key rather than as the
    // keystore problem it is.
    if chain.certs().is_empty() {
        return Err(TlsError::KeyWithoutCertificate { path: path.to_path_buf(), alias: alias.to_string() });
    }
    debug!("Presenting the client certificate \"{alias}\" from {}", path.display());
    Ok(Identity {
        chain: chain.certs().iter().map(|c| c.as_der().to_vec()).collect(),
        key: chain.key().as_der().to_vec(),
    })
}

/// Read and decrypt a store, trying the contract's default passwords when none is configured.
fn open(
    source: &Source,
    kind: &'static str,
    properties: &Properties,
    password_property: &'static str,
    policy: Pkcs12ImportPolicy,
) -> Result<KeyStore> {
    let path = source.path();
    let data = std::fs::read(path).map_err(|source| TlsError::Read { kind, path: path.to_path_buf(), source })?;

    // A configured password is the only one tried: falling back would turn a typo into a confusing
    // "wrong password" for a password the user never set. Blank counts as unset, as everywhere else
    // in the configuration, so `…truststorePassword=` in a properties file does not disable the
    // defaults.
    let candidates: Vec<&str> = match properties.get_non_blank(password_property) {
        Some(configured) => vec![configured],
        None => DEFAULT_PASSWORDS.to_vec(),
    };

    let mut last_error = None;
    for password in &candidates {
        match KeyStore::from_pkcs12(&data, password, policy) {
            Ok(store) => return Ok(store),
            Err(error) => last_error = Some(error),
        }
    }

    // Every candidate failed. Which failure it was decides the message, and the variant is matched
    // rather than its text: "Unsupported MAC algorithm" and the wrong-password "MAC tag mismatch"
    // both contain "mac", so a substring test sends the user of a SHA-384-MAC'd file hunting a
    // password problem that does not exist.
    match last_error {
        // The MAC is computed over the content with a key derived from the password, so a mismatch
        // is a wrong password. Unpadding and PBES2 failures are the same story for a file with no MAC.
        Some(
            p12_keystore::error::Error::MacError(_)
            | p12_keystore::error::Error::UnpadError
            | p12_keystore::error::Error::Pkcs5Error(_),
        ) => Err(TlsError::Password { kind, path: path.to_path_buf(), password_property }),
        other => Err(TlsError::Malformed {
            kind,
            path: path.to_path_buf(),
            message: other.map(|e| e.to_string()).unwrap_or_default(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn properties(pairs: &[(&str, &str)]) -> Properties {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    #[test]
    fn nothing_configured_and_no_default_store_leaves_the_platform_verifier_alone() {
        let home = tempfile::tempdir().unwrap();
        let stores = resolve(&properties(&[]), home.path()).unwrap();
        assert!(stores.roots.is_none(), "a root list would replace the platform verifier");
        assert!(stores.client_identity.is_none());
    }

    #[test]
    fn a_configured_path_that_does_not_exist_is_an_error() {
        let home = tempfile::tempdir().unwrap();
        let missing = home.path().join("nope.p12");
        let error = resolve(&properties(&[(TRUSTSTORE_PATH, &missing.display().to_string())]), home.path())
            .expect_err("a truststore the user named but that is absent must be reported");
        assert!(matches!(error, TlsError::Read { .. }), "got {error:?}");
    }

    #[test]
    fn the_default_location_is_probed_under_the_user_home() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(SSL_DIRECTORY)).unwrap();
        // Not a PKCS#12 file, which is the point: reaching a parse error proves it was picked up.
        std::fs::write(home.path().join(SSL_DIRECTORY).join(DEFAULT_TRUSTSTORE), b"not a keystore").unwrap();

        let error = resolve(&properties(&[]), home.path())
            .expect_err("a truststore.p12 in the default location must be read without being configured");
        let rendered = error.to_string();
        assert!(rendered.contains(DEFAULT_TRUSTSTORE), "{rendered}");
    }

    #[test]
    fn a_default_store_that_is_a_directory_is_not_mistaken_for_one() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(SSL_DIRECTORY).join(DEFAULT_KEYSTORE)).unwrap();
        let stores = resolve(&properties(&[]), home.path()).unwrap();
        assert!(stores.client_identity.is_none());
    }

    /// A PKCS#12 file holding only certificates — a truststore, as `keytool -importcert` writes one.
    fn truststore(password: &str, count: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut store = KeyStore::new();
        let mut ders = Vec::new();
        for index in 0..count {
            let issued = rcgen::generate_simple_self_signed(vec![format!("ca-{index}.example")]).unwrap();
            let der = issued.cert.der().to_vec();
            store.add_entry(
                &format!("ca-{index}"),
                KeyStoreEntry::Certificate(p12_keystore::Certificate::from_der(&der).unwrap()),
            );
            ders.push(der);
        }
        (store.writer(password).write().unwrap(), ders)
    }

    /// A PKCS#12 file holding a private key and its certificate — a keystore.
    fn keystore(password: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let issued = rcgen::generate_simple_self_signed(vec!["client.example".to_string()]).unwrap();
        let cert_der = issued.cert.der().to_vec();
        let key_der = issued.signing_key.serialize_der();

        let mut store = KeyStore::new();
        let chain = p12_keystore::PrivateKeyChain::new(
            vec![0u8; 8],
            p12_keystore::PrivateKey::from_der(&key_der).unwrap(),
            vec![p12_keystore::Certificate::from_der(&cert_der).unwrap()],
        );
        store.add_entry("client", KeyStoreEntry::PrivateKeyChain(chain));
        (store.writer(password).write().unwrap(), cert_der, key_der)
    }

    #[test]
    fn a_truststore_widens_the_platform_roots_rather_than_replacing_them() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("truststore.p12");
        let (bytes, expected) = truststore("changeit", 2);
        std::fs::write(&path, &bytes).unwrap();

        let stores = resolve(&properties(&[(TRUSTSTORE_PATH, &path.display().to_string())]), home.path()).unwrap();
        let roots = stores.roots.expect("a truststore must produce a root list");

        for certificate in &expected {
            assert!(roots.contains(certificate), "a truststore certificate is missing from the root list");
        }
        // The whole point of the contract's wording: the OS roots are still there, so the scanner can
        // still reach a public endpoint.
        assert!(
            roots.len() > expected.len(),
            "expected the operating system's roots alongside the truststore's {} certificate(s), got {}",
            expected.len(),
            roots.len()
        );
    }

    /// A truststore written by `openssl pkcs12 -export -nokeys`, which is how anyone without a JDK
    /// makes one, must be read.
    ///
    /// This cannot be expressed with `p12-keystore`'s writer, which stamps every certificate bag
    /// with Java's Oracle trusted-key-usage attribute. A store without that attribute is a different
    /// file, so it is committed rather than generated: certificates only, no private key, and a
    /// hundred-year validity so the fixture does not expire.
    #[test]
    fn a_truststore_written_by_openssl_is_read() {
        let home = tempfile::tempdir().unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ssl/openssl-truststore.p12");

        let stores = resolve(&properties(&[(TRUSTSTORE_PATH, &path.display().to_string())]), home.path())
            .expect("an openssl-written truststore must be read, not reported as empty");
        assert!(stores.roots.is_some_and(|roots| !roots.is_empty()));
    }

    #[test]
    fn a_keystore_yields_the_client_certificate_and_its_key() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("keystore.p12");
        let (bytes, cert_der, key_der) = keystore("changeit");
        std::fs::write(&path, &bytes).unwrap();

        let stores = resolve(&properties(&[(KEYSTORE_PATH, &path.display().to_string())]), home.path()).unwrap();
        let identity = stores.client_identity.expect("a keystore must produce a client identity");
        assert_eq!(identity.chain, vec![cert_der]);
        assert_eq!(identity.key, key_der, "the key must come back as the PKCS#8 DER it went in as");
        // A keystore is not a truststore: it must not silently widen who we trust.
        assert!(stores.roots.is_none());
    }

    #[test]
    fn both_default_passwords_are_tried_when_none_is_configured() {
        for password in DEFAULT_PASSWORDS {
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("truststore.p12");
            std::fs::write(&path, truststore(password, 1).0).unwrap();

            let stores = resolve(&properties(&[(TRUSTSTORE_PATH, &path.display().to_string())]), home.path())
                .unwrap_or_else(|e| panic!("a store protected with the default {password:?} must open: {e}"));
            assert!(stores.roots.is_some());
        }
    }

    #[test]
    fn a_configured_password_is_used_and_a_default_is_not_substituted_for_it() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("truststore.p12");
        std::fs::write(&path, truststore("s3cret", 1).0).unwrap();
        let configured = properties(&[(TRUSTSTORE_PATH, &path.display().to_string()), (TRUSTSTORE_PASSWORD, "s3cret")]);
        assert!(resolve(&configured, home.path()).unwrap().roots.is_some());

        // The same store, with the password left out, must not open: `changeit` is not a fallback for
        // a store the user protected with something else.
        let error = resolve(&properties(&[(TRUSTSTORE_PATH, &path.display().to_string())]), home.path())
            .expect_err("the default passwords must not open a store protected with another");
        assert!(matches!(error, TlsError::Password { .. }), "got {error:?}");
    }

    #[test]
    fn a_keystore_holding_no_private_key_is_rejected_as_one() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("certs-only.p12");
        std::fs::write(&path, truststore("changeit", 1).0).unwrap();

        let error = resolve(&properties(&[(KEYSTORE_PATH, &path.display().to_string())]), home.path())
            .expect_err("a keystore with no key cannot present a client certificate");
        let rendered = error.to_string();
        assert!(matches!(error, TlsError::NoPrivateKey { .. }), "got {error:?}");
        // The message should send the user to the right property rather than just refusing.
        assert!(rendered.contains(TRUSTSTORE_PATH), "{rendered}");
    }

    #[test]
    fn a_blank_configured_password_falls_back_to_the_defaults() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("truststore.p12");
        std::fs::write(&path, truststore("changeit", 1).0).unwrap();

        // `sonar.scanner.truststorePassword=` in a properties file, or an empty `-D`. Blank means
        // unset everywhere else in the configuration, so it must not switch the defaults off here.
        let stores = resolve(
            &properties(&[(TRUSTSTORE_PATH, &path.display().to_string()), (TRUSTSTORE_PASSWORD, "   ")]),
            home.path(),
        )
        .expect("a blank password must not disable the default-password fallback");
        assert!(stores.roots.is_some());
    }

    #[test]
    fn a_key_without_its_certificate_names_the_real_problem() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("keystore.p12");

        // A key whose localKeyID matches no certificate: valid PKCS#12, useless as a client identity.
        let issued = rcgen::generate_simple_self_signed(vec!["orphan.example".to_string()]).unwrap();
        let mut store = KeyStore::new();
        store.add_entry(
            "orphan",
            KeyStoreEntry::PrivateKeyChain(p12_keystore::PrivateKeyChain::new(
                vec![9u8; 8],
                p12_keystore::PrivateKey::from_der(&issued.signing_key.serialize_der()).unwrap(),
                Vec::new(),
            )),
        );
        std::fs::write(&path, store.writer("changeit").write().unwrap()).unwrap();

        let error = resolve(&properties(&[(KEYSTORE_PATH, &path.display().to_string())]), home.path())
            .expect_err("a key with no certificate cannot be presented");
        assert!(matches!(error, TlsError::KeyWithoutCertificate { .. }), "got {error:?}");
    }

    #[test]
    fn the_password_is_not_guessed_when_one_is_configured() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("store.p12");
        std::fs::write(&path, b"not a keystore").unwrap();
        let error = resolve(
            &properties(&[(TRUSTSTORE_PATH, &path.display().to_string()), (TRUSTSTORE_PASSWORD, "hunter2")]),
            home.path(),
        )
        .expect_err("garbage is not a keystore whatever the password");
        // Whichever arm it lands in, the secret must not be in the message.
        assert!(!error.to_string().contains("hunter2"), "the password leaked into {error}");
    }
}
