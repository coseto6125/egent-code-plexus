use std::fmt;

#[derive(Debug, Clone)]
pub struct User {
    pub id: u64,
    pub email: String,
    pub name: String,
    pub role: String,
}

impl User {
    pub fn new(id: u64, email: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id,
            email: email.into(),
            name: name.into(),
            role: "user".to_string(),
        }
    }

    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

impl fmt::Display for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} <{}>", self.name, self.email)
    }
}
