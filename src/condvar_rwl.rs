#[derive(Default)]
pub struct CondvarRwl {
    c: parking_lot::Condvar,
    m: parking_lot::Mutex<()>,
}
impl CondvarRwl {
    pub fn new() -> Self {
        Default::default()
    }
    pub fn wait_while_for<T>(
        &self,
        g: &mut parking_lot::RwLockWriteGuard<'_, T>,
        timeout: std::time::Duration,
        mut condition: impl FnMut() -> bool,
    ) {
        let guard = self.m.lock();
        parking_lot::RwLockWriteGuard::unlocked(g, || {
            let mut guard = guard;
            self.c.wait_while_for(&mut guard, |_| condition(), timeout);
        });
    }
    pub fn notify_all(&self) -> usize {
        self.c.notify_all()
    }
}
