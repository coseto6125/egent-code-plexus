namespace Example
{
    public class User
    {
        public long Id { get; set; }
        public string Email { get; set; } = string.Empty;
        public string Name { get; set; } = string.Empty;
        public string Role { get; set; } = "user";

        public bool IsAdmin() => Role == "admin";
        public string DisplayName() => $"{Name} <{Email}>";
    }
}
