<?php

namespace App\Utils;

class StringUtils
{
    public static function slugify(string $text): string
    {
        $lower = strtolower($text);
        $slug = preg_replace('/[^a-z0-9]+/', '-', $lower);
        return trim($slug, '-');
    }

    public static function isValidEmail(string $email): bool
    {
        return filter_var($email, FILTER_VALIDATE_EMAIL) !== false;
    }

    public static function truncate(string $text, int $maxLen): string
    {
        return strlen($text) <= $maxLen ? $text : substr($text, 0, $maxLen) . '...';
    }
}
