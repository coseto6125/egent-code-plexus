module Config
  def self.load
    {
      debug: ENV.fetch('DEBUG', 'false') == 'true',
      secret_key: ENV.fetch('SECRET_KEY', 'dev-secret'),
      database: {
        host: ENV.fetch('DB_HOST', 'localhost'),
        port: ENV.fetch('DB_PORT', '5432').to_i,
        name: ENV.fetch('DB_NAME', 'app')
      }
    }
  end

  def self.env_or(key, default)
    ENV.fetch(key, default)
  end
end
