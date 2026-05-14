module Greetable
  def greet(name : String) : String
    "Hello, #{name}!"
  end
end

class Helper
  include Greetable

  VERSION = "1.0.0"

  def format_message(msg : String) : String
    "[Helper] #{msg}"
  end
end
