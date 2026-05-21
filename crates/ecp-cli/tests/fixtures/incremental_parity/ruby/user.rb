class User
  attr_reader :id, :email, :name, :role

  def initialize(id, email, name, role = 'user')
    @id = id
    @email = email
    @name = name
    @role = role
  end

  def admin?
    @role == 'admin'
  end

  def display_name
    "#{@name} <#{@email}>"
  end
end
