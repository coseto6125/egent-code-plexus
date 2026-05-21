package com.example

fun String.slugify(): String =
    lowercase().replace(Regex("[^a-z0-9]+"), "-").trim('-')

fun String.isValidEmail(): Boolean =
    contains('@') && substringAfter('@').contains('.')

fun <T> List<T>.paginate(page: Int, perPage: Int): List<T> {
    val start = (page - 1) * perPage
    return if (start >= size) emptyList() else subList(start, minOf(start + perPage, size))
}

fun String.truncate(maxLen: Int, ellipsis: String = "..."): String =
    if (length <= maxLen) this else take(maxLen) + ellipsis
