String slugify(String text) {
  return text
      .toLowerCase()
      .replaceAll(RegExp(r'[^a-z0-9]+'), '-')
      .replaceAll(RegExp(r'^-|-$'), '');
}

bool isValidEmail(String email) {
  final at = email.indexOf('@');
  if (at < 1) return false;
  return email.substring(at + 1).contains('.');
}

String truncate(String text, int maxLen) {
  if (text.length <= maxLen) return text;
  return '${text.substring(0, maxLen)}...';
}

List<T> paginate<T>(List<T> items, int page, int perPage) {
  final start = (page - 1) * perPage;
  if (start >= items.length) return [];
  return items.sublist(start, (start + perPage).clamp(0, items.length));
}
