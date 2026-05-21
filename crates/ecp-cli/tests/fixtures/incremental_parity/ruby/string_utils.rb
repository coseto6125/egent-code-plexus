module StringUtils
  def self.slugify(text)
    text.downcase.gsub(/[^a-z0-9]+/, '-').gsub(/^-|-$/, '')
  end

  def self.valid_email?(email)
    email.include?('@') && email.split('@').last.include?('.')
  end

  def self.truncate(text, max_len)
    text.length <= max_len ? text : "#{text[0, max_len]}..."
  end

  def self.capitalize_words(text)
    text.split.map(&:capitalize).join(' ')
  end
end
