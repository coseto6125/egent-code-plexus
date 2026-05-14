require "./helper"

APP_NAME = "CrystalApp"

class App < Helper
  def initialize(@name : String)
  end

  def run : Int32
    msg = format_message(greet(@name))
    puts msg
    0
  end

  def self.start(name : String) : App
    app = App.new(name)
    app.run
    app
  end
end

app = App.start(APP_NAME)
