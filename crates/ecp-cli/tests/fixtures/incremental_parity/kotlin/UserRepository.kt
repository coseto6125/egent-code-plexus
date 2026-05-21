package com.example

data class User(val id: Long, val email: String, val name: String)

interface UserRepository {
    fun findById(id: Long): User?
    fun findAll(): List<User>
    fun save(user: User): User
    fun delete(id: Long)
}

class InMemoryUserRepository : UserRepository {
    private val store = mutableMapOf<Long, User>()

    override fun findById(id: Long): User? = store[id]
    override fun findAll(): List<User> = store.values.toList()
    override fun save(user: User): User { store[user.id] = user; return user }
    override fun delete(id: Long) { store.remove(id) }
}
