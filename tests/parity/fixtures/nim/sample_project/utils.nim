## Reusable utility procs and types for the sample project.

type
  Point* = object
    x*: float
    y*: float

proc distance*(a, b: Point): float =
  let dx = a.x - b.x
  let dy = a.y - b.y
  result = sqrt(dx * dx + dy * dy)

func clamp*(value, lo, hi: float): float =
  if value < lo: lo
  elif value > hi: hi
  else: value

iterator range*(start, stop: int): int =
  var i = start
  while i < stop:
    yield i
    inc i
