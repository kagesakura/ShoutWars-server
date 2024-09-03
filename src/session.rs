#![allow(non_camel_case_types)]

#[derive(Clone)]
pub struct session_t {
    pub id: uuid::Uuid,
    pub room_id: uuid::Uuid,
    pub user_id: uuid::Uuid,
}

impl session_t {
    pub fn new(room_id: uuid::Uuid, user_id: uuid::Uuid) -> Self {
        Self {
            id: Self::gen_id(),
            room_id,
            user_id,
        }
    }
    fn gen_id() -> uuid::Uuid {
        uuid::Uuid::now_v7()
    }
}

pub struct session_list_t {
    pub log_error: crate::Logger,
    pub log_info: crate::Logger,
    sessions: parking_lot::RwLock<std::collections::BTreeMap<uuid::Uuid, session_t>>,
}

impl session_list_t {
    pub fn new(log_error: crate::Logger, log_info: crate::Logger) -> Self {
        Self {
            log_error,
            log_info,
            sessions: Default::default(),
        }
    }
    pub fn create(&self, room_id: uuid::Uuid, user_id: uuid::Uuid) -> session_t {
        let mut sessions = self.sessions.write();
        let session = session_t::new(room_id, user_id);
        sessions.insert(session.id, session.clone());
        (self.log_info)(&format!(
            "Session created: {} (room_id={}, user_id={})",
            session.id,
            room_id,
            user_id
        ));
        session
    }
    pub fn get(&self, id: &uuid::Uuid) -> Result<session_t, crate::AgError> {
        let sessions = self.sessions.read();
        sessions
            .get(id)
            .cloned()
            .ok_or_else(|| crate::AgError::unauthorized_error("Session not found."))
    }
    pub fn exists(&self, id: &uuid::Uuid) -> bool {
        let sessions = self.sessions.read();
        sessions.contains_key(id)
    }
    pub fn remove(&self, id: &uuid::Uuid) -> bool {
        let mut sessions = self.sessions.write();
        if sessions.remove(id).is_some() {
            (self.log_info)(&format!("Session removed: {}", id));
            return true;
        }
        return false;
    }
    pub fn clean<'a>(&self, is_expired: &(impl Fn(&session_t) -> bool + 'a)) -> usize {
        let mut sessions = self.sessions.write();
        let mut count = 0;
        sessions.retain(|id, session| {
            if is_expired(session) {
                (self.log_info)(&format!("Session expired: {}", id));
                count += 1;
                return false;
            }
            return true;
        });
        count
    }
}
