#![allow(non_camel_case_types)]

use std::*;

struct room_list_inner {
    limit: usize,
    rooms: collections::BTreeMap<uuid::Uuid, sync::Arc<crate::room_t>>,
    name_to_id: collections::BTreeMap<String, uuid::Uuid>,
}

pub struct room_list_t {
    pub log_error: crate::Logger,
    pub log_info: crate::Logger,
    pub lobby_lifetime: time::Duration,
    pub game_lifetime: time::Duration,
    rooms_mutex: parking_lot::RwLock<room_list_inner>,
}

impl room_list_t {
    const NAME_LENGTH: u32 = 6; // name is actually a 6-digit number

    pub fn new(
        limit: usize,
        lobby_lifetime: time::Duration,
        game_lifetime: time::Duration,
        log_error: crate::Logger,
        log_info: crate::Logger,
    ) -> Self {
        Self {
            log_error,
            log_info,
            lobby_lifetime,
            game_lifetime,
            rooms_mutex: parking_lot::RwLock::new(room_list_inner {
                limit,
                rooms: Default::default(),
                name_to_id: Default::default(),
            }),
        }
    }

    pub fn create(
        &self,
        version: &str,
        owner: crate::user_t,
        size: usize,
    ) -> Result<sync::Arc<crate::room_t>, crate::AgError> {
        let mut lock = self.rooms_mutex.write();
        if lock.rooms.len() >= lock.limit {
            return Err(crate::AgError::ForbiddenError(format!(
                "Room limit reached. Max room count is {}.",
                lock.limit
            )));
        }
        thread_local! {
            static GEN_RAND: cell::RefCell<rand_mt::Mt19937GenRand64> = Default::default();
            static DIST: cell::RefCell<rand::distributions::Uniform<u64>> = cell::RefCell::new(rand::distributions::Uniform::from(0 .. 10u64.pow(room_list_t::NAME_LENGTH)));
        }
        let name = loop {
            let name = format!(
                concat!("{:0", "6", "}"),
                DIST.with_borrow(|dist| GEN_RAND.with_borrow_mut(|gen_rand| {
                    rand::distributions::Distribution::sample(dist, gen_rand)
                }))
            );
            let mut buf = String::new();
            for _ in 0..(name.len() - (Self::NAME_LENGTH as usize)) {
                buf.push('0');
            }
            buf.push_str(&name);
            let name = buf;
            if !lock.name_to_id.contains_key(&name) {
                break name;
            }
        };
        let owner_id = owner.id;
        let room = sync::Arc::new(crate::room_t::new(
            version.to_owned(),
            owner,
            name.clone(),
            size,
            self.lobby_lifetime,
            self.game_lifetime,
            self.log_error.clone(),
            self.log_info.clone(),
        )?);
        *lock.rooms.get_mut(&room.id).unwrap() = room.clone();
        *lock.name_to_id.get_mut(&name).unwrap() = room.id;
        (self.log_info)(&format!(
            "Room created: {} (version={}, owner_id={}, name={}, size={})",
            room.id, version, owner_id, name, size
        ));
        Ok(room)
    }

    pub fn get_by_id(&self, id: &uuid::Uuid) -> Result<sync::Arc<crate::room_t>, crate::AgError> {
        let lock = self.rooms_mutex.read();
        lock.rooms
            .get(id)
            .cloned()
            .ok_or_else(|| crate::AgError::NotFoundError("Room not found.".to_owned()))
    }

    pub fn get(&self, name: &str) -> Result<sync::Arc<crate::room_t>, crate::AgError> {
        let lock = self.rooms_mutex.read();
        lock.name_to_id
            .get(name)
            .and_then(|id| lock.rooms.get(id))
            .cloned()
            .ok_or_else(|| crate::AgError::NotFoundError("Room not found.".to_owned()))
    }

    pub fn exists_by_id(&self, id: &uuid::Uuid) -> bool {
        let lock = self.rooms_mutex.read();
        return lock.rooms.contains_key(id);
    }

    pub fn exists(&self, name: &str) -> bool {
        let lock = self.rooms_mutex.read();
        return lock.name_to_id.contains_key(name);
    }

    pub fn remove(&self, id: &uuid::Uuid) -> bool {
        let mut lock = self.rooms_mutex.write();
        let name = lock.rooms.get(id).unwrap().name.clone();
        lock.name_to_id.remove(&name);
        if lock.rooms.remove(id).is_some() {
            (self.log_info)(&format!("Room removed: {}", id));
            return true;
        }
        return false;
    }

    pub fn count(&self) -> usize {
        let lock = self.rooms_mutex.read();
        lock.rooms.len()
    }

    pub fn get_all(&self) -> Vec<sync::Arc<crate::room_t>> {
        let lock = self.rooms_mutex.read();
        lock.rooms.values().cloned().collect()
    }

    pub fn get_limit(&self) -> usize {
        let lock = self.rooms_mutex.read();
        lock.limit
    }

    pub fn set_limit(&self, new_limit: usize) {
        let mut lock = self.rooms_mutex.write();
        lock.limit = new_limit;
    }

    pub fn clean(&self, user_timeout: time::Duration) {
        for room in self.get_all() {
            if !room.is_available() {
                self.remove(&room.id);
            }
            room.kick_expired(user_timeout);
            room.clean_sync_records();
        }
    }
}
