package main

import (
	"regexp"
	"strings"
)

var nonAlpha = regexp.MustCompile(`[^a-z0-9]+`)

func Slugify(text string) string {
	lower := strings.ToLower(text)
	return strings.Trim(nonAlpha.ReplaceAllString(lower, "-"), "-")
}

func Truncate(text string, maxLen int) string {
	if len(text) <= maxLen {
		return text
	}
	return text[:maxLen] + "..."
}

func IsValidEmail(email string) bool {
	at := strings.LastIndex(email, "@")
	if at < 1 {
		return false
	}
	return strings.Contains(email[at+1:], ".")
}
