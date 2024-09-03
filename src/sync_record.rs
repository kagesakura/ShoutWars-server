#![allow(non_camel_case_types)]

use std::*;

pub struct event_t {
    pub id: uuid::Uuid,
    pub from: uuid::Uuid,
    pub type_: String,
    pub data: serde_json::Value,
}

impl serde::Serialize for event_t {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        let mut d = serializer.serialize_struct("event_t", 4)?;
        d.serialize_field("id", &self.id.to_string())?;
        d.serialize_field("from", &self.from.to_string())?;
        d.serialize_field("type", &self.type_)?;
        d.serialize_field("data", &self.data)?;
        d.end()
    }
}

impl event_t {
    pub fn new(id: uuid::Uuid, from: uuid::Uuid, type_: String, data: serde_json::Value) -> Self {
        Self {
            id,
            from,
            type_,
            data,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum phase_t {
    CREATED = 0,
    WAITING = 1,
    SYNCING = 2,
    SYNCED = 3,
}

#[derive(Default)]
struct sync_record_inner {
    reports: collections::BTreeMap<uuid::Uuid, sync::Arc<event_t>>,
    actions: collections::BTreeMap<uuid::Uuid, sync::Arc<event_t>>,
    users_phase: collections::BTreeMap<uuid::Uuid, phase_t>,
}

pub struct sync_record_t {
    pub id: uuid::Uuid,
    inner: parking_lot::RwLock<sync_record_inner>,
}

impl sync_record_t {
    fn gen_id() -> uuid::Uuid {
        uuid::Uuid::now_v7()
    }

    pub fn new() -> Self {
        Self {
            id: Self::gen_id(),
            inner: Default::default(),
        }
    }

    pub fn add_events(
        &self,
        from: uuid::Uuid,
        new_reports: &Vec<sync::Arc<event_t>>,
        new_actions: &Vec<sync::Arc<event_t>>,
    ) -> Result<(), crate::AgError> {
        let mut lock = self.inner.write();
        if *lock.users_phase.entry(from).or_insert(phase_t::CREATED) > phase_t::CREATED {
            return Err(crate::AgError::BadRequestError(
                "Record already synced.".to_owned(),
            ));
        }
        for report in new_reports {
            if report.from != from {
                return Err(crate::AgError::BadRequestError(
                    "Invalid report from.".to_owned(),
                ));
            }
            lock.reports.insert(report.id, report.clone());
        }
        for action in new_actions {
            if action.from != from {
                return Err(crate::AgError::BadRequestError(
                    "Invalid action from.".to_owned(),
                ));
            }
            lock.reports.insert(action.id, action.clone());
        }
        lock.users_phase.insert(from, phase_t::WAITING);
        Ok(())
    }

    pub fn get_reports(&self) -> Vec<sync::Arc<event_t>> {
        let lock = self.inner.read();
        lock.reports.values().cloned().collect()
    }

    pub fn get_actions(&self) -> Vec<sync::Arc<event_t>> {
        let lock = self.inner.read();
        lock.actions.values().cloned().collect()
    }

    pub fn get_phase(&self, user_id: uuid::Uuid) -> phase_t {
        let mut lock = self.inner.write();
        return *lock.users_phase.entry(user_id).or_insert(phase_t::CREATED);
    }

    pub fn advance_phase(&self, user_id: uuid::Uuid, new_phase: phase_t) -> bool {
        let mut lock = self.inner.write();
        if new_phase <= *lock.users_phase.entry(user_id).or_insert(phase_t::CREATED) {
            return false;
        }
        lock.users_phase.insert(user_id, new_phase);
        return true;
    }

    pub fn get_max_phase(&self) -> phase_t {
        let lock = self.inner.read();
        *lock.users_phase.values().max().unwrap()
    }
}
