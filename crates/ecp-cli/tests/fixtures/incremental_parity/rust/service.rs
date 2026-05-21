use crate::models::User;
use std::collections::HashMap;

pub trait UserRepository {
    fn find_by_id(&self, id: u64) -> Option<&User>;
    fn find_all(&self) -> Vec<&User>;
    fn save(&mut self, user: User) -> &User;
    fn delete(&mut self, id: u64) -> bool;
}

pub struct InMemoryRepo {
    store: HashMap<u64, User>,
}

impl InMemoryRepo {
    pub fn new() -> Self {
        Self { store: HashMap::new() }
    }
}

impl UserRepository for InMemoryRepo {
    fn find_by_id(&self, id: u64) -> Option<&User> {
        self.store.get(&id)
    }

    fn find_all(&self) -> Vec<&User> {
        self.store.values().collect()
    }

    fn save(&mut self, user: User) -> &User {
        self.store.insert(user.id, user);
        self.store.get(&user.id).unwrap()
    }

    fn delete(&mut self, id: u64) -> bool {
        self.store.remove(&id).is_some()
    }
}
