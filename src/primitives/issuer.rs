//! Generates bearer tokens and refresh tokens.
//!
//! Internally similar to the authorization module, tokens generated here live longer and can be
//! renewed. There exist two fundamental implementation as well, one utilizing in memory hash maps
//! while the other uses cryptographic signing.
use std::collections::HashMap;
use std::sync::{MutexGuard, RwLockWriteGuard};
use std::sync::atomic::{ATOMIC_USIZE_INIT, AtomicUsize, Ordering};

use super::Time;
use super::grant::Grant;
use super::generator::{TagGrant, Assertion};

/// Issuers create bearer tokens.
///
/// It's the issuers decision whether a refresh token is offered or not. In any case, it is also
/// responsible for determining the validity and parameters of any possible token string. Some
/// backends or frontends may decide not to propagate the refresh token (for example because
/// they do not intend to offer a statefull refresh api).
pub trait Issuer {
    /// Create a token authorizing the request parameters
    fn issue(&mut self, Grant) -> Result<IssuedToken, ()>;

    /// Get the values corresponding to a bearer token
    fn recover_token<'a>(&'a self, &'a str) -> Result<Option<Grant>, ()>;

    /// Get the values corresponding to a refresh token
    fn recover_refresh<'a>(&'a self, &'a str) -> Result<Option<Grant>, ()>;
}

/// Token parameters returned to a client.
#[derive(Clone, Debug)]
pub struct IssuedToken {
    /// The bearer token
    pub token: String,

    /// The refresh token
    pub refresh: String,

    /// Expiration timestamp (Utc).
    ///
    /// Technically, a time to live is expected in the response but this will be transformed later.
    /// In a direct backend access situation, this enables high precision timestamps.
    pub until: Time,
}

/// Keeps track of access and refresh tokens by a hash-map.
///
/// The generator is itself trait based and can be chosen during construction. It is assumed to not
/// be possible (or at least very unlikely during their overlapping lifetime) for two different
/// grants to generate the same token in the grant tagger.
pub struct TokenMap<G: TagGrant=Box<TagGrant + Send + Sync + 'static>> {
    generator: G,
    usage: u64,
    access: HashMap<String, Grant>,
    refresh: HashMap<String, Grant>,
}

impl<G: TagGrant> TokenMap<G> {
    /// Construct a `TokenMap` from the given generator.
    pub fn new(generator: G) -> Self {
        Self {
            generator,
            usage: 0,
            access: HashMap::new(),
            refresh: HashMap::new(),
        }
    }
}

impl<G: TagGrant> Issuer for TokenMap<G> {
    fn issue(&mut self, grant: Grant) -> Result<IssuedToken, ()> {
        // The (usage, grant) tuple needs to be unique. Since this wraps after 2^63 operations, we
        // expect the validity time of the grant to have changed by then. This works when you don't
        // set your system time forward/backward ~10billion seconds, assuming ~10^9 operations per
        // second.
        let next_usage = self.usage.wrapping_add(2);

        let (token, refresh) = {
            let token = self.generator.tag(next_usage - 2, &grant)?;
            let refresh = self.generator.tag(next_usage - 1, &grant)?;
            (token, refresh)
        };

        let until = grant.until.clone();
        self.access.insert(token.clone(), grant.clone());
        self.refresh.insert(refresh.clone(), grant);
        self.usage = next_usage;
        Ok(IssuedToken { token, refresh, until })
    }

    fn recover_token<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        Ok(self.access.get(token).cloned())
    }

    fn recover_refresh<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        Ok(self.refresh.get(token).cloned())
    }
}

/// Signs grants instead of storing them.
///
/// Although this token instance allows preservation of memory it also implies that tokens, once
/// issued, are impossible to revoke.
pub struct TokenSigner {
    signer: Assertion,
    // FIXME: make this an AtomicU64 once stable.
    counter: AtomicUsize,
}

impl TokenSigner {
    /// Construct a signing instance from a private signing key.
    ///
    /// Security notice: Never use a password alone to construct the signing key. Instead, generate
    /// a new key using a utility such as `openssl rand` that you then store away securely.
    pub fn new<S: Into<Assertion>>(secret: S) -> TokenSigner {
        TokenSigner { 
            signer: secret.into(),
            counter: ATOMIC_USIZE_INIT,
        }
    }

    /// Construct a signing instance whose tokens only live for the program execution.
    ///
    /// Useful for rapid prototyping where tokens need not be stored in a persistent database and
    /// can be invalidated at any time. This interface is provided with simplicity in mind, using
    /// the default system random generator (`ring::rand::SystemRandom`). If you want an ephemeral
    /// key but more customization, adapt the implementation.
    ///
    /// ```
    /// # use oxide_auth::primitives::issuer::TokenSigner;
    /// TokenSigner::new(
    ///     ring::hmac::SigningKey::generate(
    ///         &ring::digest::SHA256, 
    ///         &mut ring::rand::SystemRandom::new())
    ///     .unwrap());
    /// ```
    pub fn ephemeral() -> TokenSigner {
        TokenSigner::new(Assertion::ephermal())
    }

    /// Get the next counter value.
    fn next_counter(&self) -> usize {
        // Acquire+Release is overkill. We only need to ensure that each return value occurs at
        // most once. We would even be content with getting the counter out-of-order in a single
        // thread.
        self.counter.fetch_add(1, Ordering::Relaxed)
    }
}

impl<'s, I: Issuer + ?Sized> Issuer for &'s mut I {
    fn issue(&mut self, grant: Grant) -> Result<IssuedToken, ()> {
        (**self).issue(grant)
    }

    fn recover_token<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        (**self).recover_token(token)
    }

    fn recover_refresh<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        (**self).recover_refresh(token)
    }
}

impl<I: Issuer + ?Sized> Issuer for Box<I> {
    fn issue(&mut self, grant: Grant) -> Result<IssuedToken, ()> {
        (**self).issue(grant)
    }

    fn recover_token<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        (**self).recover_token(token)
    }

    fn recover_refresh<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        (**self).recover_refresh(token)
    }
}

impl<'s, I: Issuer + ?Sized> Issuer for MutexGuard<'s, I> {
    fn issue(&mut self, grant: Grant) -> Result<IssuedToken, ()> {
        (**self).issue(grant)
    }

    fn recover_token<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        (**self).recover_token(token)
    }

    fn recover_refresh<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        (**self).recover_refresh(token)
    }
}

impl<'s, I: Issuer + ?Sized> Issuer for RwLockWriteGuard<'s, I> {
    fn issue(&mut self, grant: Grant) -> Result<IssuedToken, ()> {
        (**self).issue(grant)
    }

    fn recover_token<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        (**self).recover_token(token)
    }

    fn recover_refresh<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        (**self).recover_refresh(token)
    }
}

impl Issuer for TokenSigner {
    fn issue(&mut self, grant: Grant) -> Result<IssuedToken, ()> {
        (&mut&*self).issue(grant)
    }

    fn recover_token<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        (&&*self).recover_token(token)
    }

    fn recover_refresh<'a>(&'a self, token: &'a str) -> Result<Option<Grant>, ()> {
        (&&*self).recover_refresh(token)
    }
}

impl<'a> Issuer for &'a TokenSigner {
    fn issue(&mut self, grant: Grant) -> Result<IssuedToken, ()> {
        let first_ctr = self.next_counter() as u64;
        let second_ctr = self.next_counter() as u64;
        let token = self.signer.tag("token").sign(first_ctr, &grant)?;
        let refresh = self.signer.tag("refresh").sign(second_ctr, &grant)?;
        Ok(IssuedToken {
            token,
            refresh,
            until: grant.until,
        })
    }

    fn recover_token<'t>(&'t self, token: &'t str) -> Result<Option<Grant>, ()> {
        Ok(self.signer.tag("token").extract(token).ok())
    }

    fn recover_refresh<'t>(&'t self, token: &'t str) -> Result<Option<Grant>, ()> {
        Ok(self.signer.tag("refresh").extract(token).ok())
    }
}

#[cfg(test)]
/// Tests for issuer implementations, including those provided here.
pub mod tests {
    use super::*;
    use primitives::grant::Extensions;
    use primitives::generator::RandomGenerator;
    use chrono::{Duration, Utc};

    /// Tests the simplest invariants that should be upheld by all authorizers.
    ///
    /// This create a token, without any extensions, an lets the issuer generate a issued token.
    /// The uri is `https://example.com` and the token lasts for an hour. Generation of a valid
    /// refresh token is not tested against.
    ///
    /// Custom implementations may want to import and use this in their own tests.
    pub fn simple_test_suite(issuer: &mut Issuer) {
        let request = Grant {
            client_id: "Client".to_string(),
            owner_id: "Owner".to_string(),
            redirect_uri: "https://example.com".parse().unwrap(),
            scope: "default".parse().unwrap(),
            until: Utc::now() + Duration::hours(1),
            extensions: Extensions::new(),
        };

        let issued = issuer.issue(request.clone())
            .expect("Issuing failed");
        let from_token = issuer.recover_token(&issued.token)
            .expect("Issuer failed during recover")
            .expect("Issued token appears to be invalid");

        assert_ne!(issued.token, issued.refresh);
        assert_eq!(from_token.client_id, "Client");
        assert_eq!(from_token.owner_id, "Owner");
        assert!(Utc::now() < from_token.until);

        let issued_2 = issuer.issue(request)
            .expect("Issuing failed");
        assert_ne!(issued.token, issued_2.token);
        assert_ne!(issued.token, issued_2.refresh);
        assert_ne!(issued.refresh, issued_2.refresh);
        assert_ne!(issued.refresh, issued_2.token);
    }

    #[test]
    fn signer_test_suite() {
        let mut signer = TokenSigner::ephemeral();
        simple_test_suite(&mut signer);
    }

    #[test]
    fn random_test_suite() {
        let mut token_map = TokenMap::new(RandomGenerator::new(16));
        simple_test_suite(&mut token_map);
    }

    #[test]
    #[should_panic]
    fn bad_generator() {
        struct BadGenerator;
        impl TagGrant for BadGenerator {
            fn tag(&mut self, _: u64, _: &Grant) -> Result<String, ()> {
                Ok("YOLO.HowBadCanItBeToRepeatTokens?".into())
            }
        }
        let mut token_map = TokenMap::new(BadGenerator);
        simple_test_suite(&mut token_map);
    }
}
