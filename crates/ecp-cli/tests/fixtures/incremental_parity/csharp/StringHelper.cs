using System;
using System.Text.RegularExpressions;

namespace Example
{
    public static class StringHelper
    {
        public static string Slugify(string text) =>
            Regex.Replace(text.ToLower(), @"[^a-z0-9]+", "-").Trim('-');

        public static bool IsValidEmail(string email) =>
            email != null && email.Contains('@') && email.Contains('.');

        public static string Truncate(string text, int maxLen) =>
            text.Length <= maxLen ? text : text[..maxLen] + "...";

        public static string Capitalize(string text) =>
            string.IsNullOrEmpty(text) ? text : char.ToUpper(text[0]) + text[1..];
    }
}
