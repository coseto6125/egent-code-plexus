use gnx_core::analyzer::types::RawRoute;

const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "options", "head", "connect", "trace",
];

#[derive(Debug, Clone)]
pub struct DetectedRoute {
    pub method: String,
    pub path: String,
}

fn looks_like_path(s: &str) -> bool {
    s.starts_with('/')
        || s.contains(':')
        || s.contains('{')
        || s.contains('<')
        // Lenient fallback for cases like [HttpGet("users")]
        || (!s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-'))
}

fn extract_string_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            let quote = c;
            let mut arg = String::new();
            while let Some(&next_c) = chars.peek() {
                if next_c == quote {
                    chars.next(); // consume closing quote
                    break;
                }
                arg.push(next_c);
                chars.next();
            }
            args.push(arg);
        }
    }
    args
}

pub fn detect_from_decorator(decorator: &str) -> Option<DetectedRoute> {
    let lower = decorator.to_lowercase();
    let method = HTTP_METHODS.iter().find(|&&m| lower.contains(m))?;

    let string_args = extract_string_args(decorator);
    let path = string_args.iter().find(|s| looks_like_path(s))?;

    Some(DetectedRoute {
        method: method.to_uppercase(),
        path: path.clone(),
    })
}

pub fn detect_from_call(raw: &RawRoute) -> Option<DetectedRoute> {
    let lower = raw.method.to_lowercase();
    let method = HTTP_METHODS.iter().find(|&&m| lower.contains(m))?;

    if looks_like_path(&raw.path) {
        Some(DetectedRoute {
            method: method.to_uppercase(),
            path: raw.path.clone(),
        })
    } else {
        None
    }
}
