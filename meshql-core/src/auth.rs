use crate::{Envelope, Stash};

pub trait Auth: Send + Sync {
    fn get_auth_token(&self, context: &Stash) -> Vec<String>;
    fn is_authorized(&self, credentials: &[String], envelope: &Envelope) -> bool;
}

pub struct NoAuth;

impl Auth for NoAuth {
    fn get_auth_token(&self, _context: &Stash) -> Vec<String> {
        vec!["*".to_string()]
    }

    fn is_authorized(&self, _credentials: &[String], _envelope: &Envelope) -> bool {
        true
    }
}
