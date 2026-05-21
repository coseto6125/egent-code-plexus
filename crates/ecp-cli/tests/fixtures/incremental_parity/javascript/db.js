class Database {
  constructor(url) {
    this.url = url;
    this.connected = false;
  }

  async connect() {
    this.connected = true;
    return this;
  }

  async query(sql, params) {
    if (!this.connected) throw new Error("Not connected");
    return [];
  }

  async close() {
    this.connected = false;
  }
}

function createPool(url, size) {
  return Array.from({ length: size }, () => new Database(url));
}

module.exports = { Database, createPool };
