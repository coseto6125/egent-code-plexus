package com.example

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

class ApiService(private val repository: UserRepository) {

    suspend fun getUser(id: Long): User? = withContext(Dispatchers.IO) {
        repository.findById(id)
    }

    suspend fun createUser(email: String, name: String): User = withContext(Dispatchers.IO) {
        val user = User(id = System.currentTimeMillis(), email = email, name = name)
        repository.save(user)
    }

    fun listUsers(): List<User> = repository.findAll()

    fun deleteUser(id: Long) = repository.delete(id)
}
