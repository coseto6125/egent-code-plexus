function handleLogin(username: string, password: string) {
    const user = lookupUser(username);
    if (!verifyPassword(user, password)) return null;
    return createSession(user);
}

function lookupUser(name: string) {
    return dbQuery(name);
}

function verifyPassword(user: any, password: string) {
    return hashPassword(password) === user.password_hash;
}

function hashPassword(password: string) {
    return `hash_${password}`;
}

function createSession(user: any) {
    return storeSession(generateSessionId(), user);
}

function generateSessionId() {
    return Math.random().toString(36);
}

function storeSession(id: string, user: any) {
    return { id, user };
}

function dbQuery(q: string) {
    return null;
}
