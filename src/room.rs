#![allow(non_camel_case_types)]

use std::*;

#[derive(Debug, Clone)]
pub struct user_t {
    pub id: uuid::Uuid,
    name: String,
    last_sync_id: uuid::Uuid,
    last_time: time::Instant,
}

impl serde::Serialize for user_t {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        let mut d = serializer.serialize_struct("event_t", 2)?;
        d.serialize_field("id", &self.id.to_string())?;
        d.serialize_field("name", &self.name)?;
        d.end()
    }
}

impl user_t {
    pub const NAME_MAX_LENGTH: usize = 32;
    pub fn new(name: &str) -> Result<Self, crate::AgError> {
        let mut this = Self {
            id: room_t::gen_id(),
            name: String::new(),
            last_sync_id: uuid::Uuid::nil(),
            last_time: time::Instant::now(),
        };
        this.set_name(name);
        Ok(this)
    }
    pub fn get_name(&self) -> String {
        todo!()
    }
    pub fn set_name(&mut self, new_name: &str) -> Result<(), crate::AgError> {
        if new_name.is_empty() || new_name.len() > Self::NAME_MAX_LENGTH {
            return Err(crate::AgError::BadRequestError(format!(
                "Invalid user name length: {}. Must be between 1 and {}.",
                new_name.len(),
                Self::NAME_MAX_LENGTH
            )));
        }
        self.name = new_name.to_owned();
        Ok(())
    }
    pub fn get_last_sync_id(&self) -> uuid::Uuid {
        self.last_sync_id
    }
    pub fn get_last_time(&self) -> time::Instant {
        self.last_time
    }
    pub fn update_last(&mut self, new_sync_id: uuid::Uuid) {
        self.last_sync_id = new_sync_id;
        self.last_time = time::Instant::now();
    }
}

struct room_inner {
    expire_time: time::Instant,
    users: collections::BTreeMap<uuid::Uuid, user_t>,
    in_lobby: bool,
    info: serde_json::Value,
    sync_records: collections::BTreeMap<uuid::Uuid, sync::Arc<crate::sync_record_t>>,
}

pub struct room_t {
    pub log_error: crate::Logger,
    pub log_info: crate::Logger,
    pub lobby_lifetime: time::Duration,
    pub game_lifetime: time::Duration,
    pub id: uuid::Uuid,
    pub version: String,
    pub name: String,
    pub size: usize,
    inner: parking_lot::RwLock<room_inner>,
    sync_cv: crate::CondvarRwl,
}

impl room_t {
    pub const VERSION_MAX_LENGTH: usize = 32;
    pub const SIZE_MAX: usize = 4;
    pub fn new(
        version: String,
        owner: user_t,
        name: String,
        size: usize,
        lobby_lifetime: time::Duration,
        game_lifetime: time::Duration,
        log_error: crate::Logger,
        log_info: crate::Logger,
    ) -> Result<Self, crate::AgError> {
        if version.is_empty() || version.len() > Self::VERSION_MAX_LENGTH {
            return Err(crate::AgError::BadRequestError(format!(
                "Invalid room version length: {}. Must be between 1 and {}.",
                version.len(),
                Self::VERSION_MAX_LENGTH
            )));
        }
        if size < 2 || size > Self::SIZE_MAX {
            return Err(crate::AgError::BadRequestError(format!(
                "Invalid room size: {}. Must be between 2 and {}.",
                size,
                Self::SIZE_MAX
            )));
        }
        let mut inner = room_inner {
            expire_time: time::Instant::now() + lobby_lifetime,
            users: collections::BTreeMap::from([(owner.id.clone(), owner)]),
            in_lobby: true,
            info: Default::default(),
            sync_records: Default::default(),
        };
        let record = sync::Arc::new(crate::sync_record_t::new());
        inner.sync_records.insert(record.id.clone(), record);
        inner
            .users
            .first_entry()
            .unwrap()
            .get_mut()
            .update_last(uuid::Uuid::nil());
        Ok(Self {
            log_error,
            log_info,
            lobby_lifetime,
            game_lifetime,
            id: Self::gen_id(),
            version,
            name,
            size,
            inner: parking_lot::RwLock::new(inner),
            sync_cv: crate::CondvarRwl::new(),
        })
    }

    pub fn get_expire_time(&self) -> time::Instant {
        let lock = self.inner.read();
        lock.expire_time.clone()
    }

    pub fn join(&self, version: String, user: user_t) -> Result<(), crate::AgError> {
        if version != self.version {
            return Err(crate::AgError::BadRequestError(format!(
                "Invalid room version: {}. This roon version is {}.",
                version, self.version
            )));
        }
        let room_inner {
            users,
            in_lobby,
            sync_records,
            ..
        } = &mut *self.inner.write();
        if !*in_lobby {
            return Err(crate::AgError::ForbiddenError(
                "Game already started.".to_owned(),
            ));
        }
        if users.len() >= self.size {
            return Err(crate::AgError::ForbiddenError(format!(
                "Room is full. Max user count is {}.",
                self.size
            )));
        }
        if users.contains_key(&user.id) {
            return Err(crate::AgError::ForbiddenError(
                "User already in the room.".to_owned(),
            ));
        }
        let user_id = user.id.clone();
        users.insert(user.id, user);
        let new_user = users.get_mut(&user_id).unwrap();
        new_user.update_last(if sync_records.len() > 1 {
            sync_records.last_key_value().unwrap().0.clone()
        } else {
            uuid::Uuid::nil()
        });
        Ok(())
    }

    pub fn get_user(&self, id: &uuid::Uuid) -> Result<user_t, crate::AgError> {
        let lock = self.inner.read();
        lock.users
            .get(id)
            .cloned()
            .ok_or_else(|| crate::AgError::NotFoundError("User not found.".to_owned()))
    }

    pub fn has_user(&self, id: &uuid::Uuid) -> bool {
        let lock = self.inner.read();
        return lock.users.contains_key(id);
    }

    pub fn kick(&self, id: &uuid::Uuid) -> bool {
        let mut lock = self.inner.write();
        return lock.users.remove(id).is_some();
    }

    pub fn kick_expired(&self, timeout: time::Duration) -> usize {
        let mut lock = self.inner.write();
        let now = time::Instant::now();
        let mut count = 0;
        lock.users.retain(|_, user| {
            if now - user.get_last_time() > timeout {
                count += 1;
                false
            } else {
                true
            }
        });
        return count;
    }

    pub fn count_users(&self) -> usize {
        let lock = self.inner.read();
        return lock.users.len();
    }

    pub fn get_user_ids(&self) -> Vec<uuid::Uuid> {
        let lock = self.inner.read();
        lock.users.keys().cloned().collect()
    }

    pub fn get_users(&self) -> Vec<user_t> {
        let lock = self.inner.read();
        lock.users.values().cloned().collect()
    }

    pub fn get_owner(&self) -> Result<user_t, crate::AgError> {
        let lock = self.inner.read();
        if lock.users.is_empty() {
            return Err(crate::AgError::NotFoundError("Room is empty.".to_owned()));
        }
        return Ok(lock.users.first_key_value().unwrap().1.clone());
    }

    pub fn is_in_lobby(&self) -> bool {
        let lock = self.inner.read();
        return lock.in_lobby;
    }

    pub fn start_game(&self) -> Result<(), crate::AgError> {
        let lock = self.inner.read();
        if !lock.in_lobby {
            return Err(crate::AgError::ForbiddenError(
                "Game already started.".to_owned(),
            ));
        }
        if lock.users.len() < 2 {
            return Err(crate::AgError::ForbiddenError(
                "Not enough players to start the game.".to_owned(),
            ));
        }
        lock.in_lobby = false;
        lock.expire_time = time::Instant::now() + self.game_lifetime;
        (self.log_info)(&format!(
            "Game started: {} (users={:?})",
            self.id.to_string(),
            lock.users.values().collect::<Vec<_>>()
        ));
        Ok(())
    }

    pub fn is_available(&self) -> bool {
        let lock = self.inner.read();
        if time::Instant::now() > lock.expire_time {
            return false;
        }
        if lock.in_lobby {
            return !lock.users.is_empty();
        }
        return lock.users.len() > 1;
    }

    pub fn get_info(&self) -> serde_json::Value {
        let lock = self.inner.read();
        return lock.info.clone();
    }

    pub fn update_info(&self, new_info: serde_json::Value) {
        let mut lock = self.inner.write();
        lock.info = new_info;
    }

    pub fn sync(
        &self,
        user_id: &uuid::Uuid,
        reports: &Vec<sync::Arc<crate::event_t>>,
        actions: &Vec<sync::Arc<crate::event_t>>,
        wait_timeout: time::Duration, // default = time::Duration::from_millis(200)
        sync_timeout: time::Duration, // default = time::Duration::from_millis(50)
    ) -> Result<Vec<sync::Arc<crate::sync_record_t>>, crate::AgError> {
        let mut lock = self.inner.write();

        if !lock.users.contains_key(user_id) {
            return Err(crate::AgError::ForbiddenError(
                "User not in the room.".to_owned(),
            ));
        }
        let record = lock.sync_records.last_key_value().unwrap().1.clone();
        if record.get_phase(user_id.clone()) > crate::phase_t::CREATED {
            return Err(crate::AgError::ForbiddenError(
                "User already synced.".to_owned(),
            ));
        }
        if record.get_max_phase() >= crate::phase_t::SYNCED {
            return Err(crate::AgError::ForbiddenError(
                "Room already synced.".to_owned(),
            ));
        }

        record.add_events(&user_id, reports, actions);

        // wait for users who didn't skip last sync
        if record.get_max_phase() <= crate::phase_t::WAITING && lock.sync_records.len() > 1 {
            if lock
                .sync_records
                .last_key_value()
                .unwrap()
                .1
                .get_phase(user_id.clone())
                < crate::phase_t::SYNCED
            {
                self.sync_cv.wait_while_for(&mut lock, wait_timeout, || {
                    !(record.get_max_phase() > crate::phase_t::WAITING)
                });
            }
        }
        record.advance_phase(&user_id, crate::phase_t::SYNCING);
        self.sync_cv.notify_all();

        // wait for all users to sync
        if lock
            .users
            .keys()
            .any(|id| record.get_phase(id.clone()) <= crate::phase_t::CREATED)
        {
            self.sync_cv.wait_while_for(&mut lock, sync_timeout, || {
                !(record.get_max_phase() > crate::phase_t::SYNCING)
            });
        }
        record.advance_phase(&user_id, crate::phase_t::SYNCED);
        self.sync_cv.notify_all();

        let user = lock.users.get(user_id).unwrap();
        let mut records = Vec::new();
        for (_, r) in lock.sync_records.range((
            ops::Bound::Excluded(user.get_last_sync_id()),
            ops::Bound::Unbounded,
        )) {
            records.push(r.clone());
            r.advance_phase(&user_id, crate::phase_t::SYNCED);
        }
        // if all users synced, create new record
        if lock.users.keys().cloned().any(|id| {
            if record.get_phase(id) <= crate::phase_t::CREATED {
                return true;
            }
            return record.get_phase(id) >= crate::phase_t::SYNCED;
        }) {
            let next_record = sync::Arc::new(crate::sync_record_t::new());
            lock.sync_records
                .insert(next_record.id.clone(), next_record);
        }

        let user = lock.users.get_mut(user_id).unwrap();
        user.update_last(record.id);
        return Ok(records);
    }

    pub fn clean_sync_records(&self) -> usize {
        let mut lock = self.inner.write();
        let mut count = 0;
        let user_ids: Vec<_> = lock.users.keys().cloned().collect();
        lock.sync_records.retain(|_, record| {
            let n = user_ids
                .iter()
                .cloned()
                .any(|id| record.get_phase(id) >= crate::phase_t::SYNCED);
            if n {
                count += 1;
            }
            return !n;
        });
        return count;
    }

    fn gen_id() -> uuid::Uuid {
        uuid::Uuid::now_v7()
    }
}
