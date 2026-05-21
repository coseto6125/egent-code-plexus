<?php

namespace App\Models;

class User
{
    public int $id;
    public string $email;
    public string $name;
    public string $role;

    public function __construct(int $id, string $email, string $name, string $role = 'user')
    {
        $this->id = $id;
        $this->email = $email;
        $this->name = $name;
        $this->role = $role;
    }

    public function isAdmin(): bool
    {
        return $this->role === 'admin';
    }

    public function displayName(): string
    {
        return "{$this->name} <{$this->email}>";
    }
}
