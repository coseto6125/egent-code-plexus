require_relative 'user'

class UserService
  def initialize(repository)
    @repository = repository
  end

  def find_by_id(id)
    @repository.find_by_id(id)
  end

  def find_all
    @repository.find_all
  end

  def create(email, name)
    user = User.new(nil, email, name)
    @repository.save(user)
  end

  def delete(id)
    @repository.delete(id)
  end
end
