using System.Collections.Generic;
using System.Linq;

namespace Example
{
    public class UserService
    {
        private readonly IUserRepository _repository;

        public UserService(IUserRepository repository)
        {
            _repository = repository;
        }

        public User GetById(long id) => _repository.FindById(id);

        public IEnumerable<User> GetAll() => _repository.FindAll();

        public User Create(string email, string name) =>
            _repository.Save(new User { Email = email, Name = name });

        public void Delete(long id) => _repository.Delete(id);
    }
}
