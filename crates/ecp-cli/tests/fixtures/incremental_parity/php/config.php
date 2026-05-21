<?php

namespace App;

function loadConfig(): array
{
    return [
        'debug' => getenv('DEBUG') === 'true',
        'secret_key' => getenv('SECRET_KEY') ?: 'dev-secret',
        'database' => [
            'host' => getenv('DB_HOST') ?: 'localhost',
            'port' => (int)(getenv('DB_PORT') ?: 5432),
            'name' => getenv('DB_NAME') ?: 'app',
        ],
    ];
}

function getEnvOrDefault(string $key, string $default): string
{
    $val = getenv($key);
    return $val !== false ? $val : $default;
}
