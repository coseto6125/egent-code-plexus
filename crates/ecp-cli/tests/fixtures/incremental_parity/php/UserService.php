<?php

namespace App\Services;

use App\Models\User;

class UserService
{
    private UserRepository $repository;

    public function __construct(UserRepository $repository)
    {
        $this->repository = $repository;
    }

    public function findById(int $id): ?User
    {
        return $this->repository->findById($id);
    }

    public function findAll(): array
    {
        return $this->repository->findAll();
    }

    public function create(string $email, string $name): User
    {
        $user = new User(0, $email, $name);
        return $this->repository->save($user);
    }

    public function delete(int $id): bool
    {
        return $this->repository->delete($id);
    }
}
