## Entry point for the Nim sample project.
## Defines a Shape type hierarchy and exercises utils.nim imports.

import strutils
from utils import distance, clamp, Point

type
  Shape* = object
    name*: string
    origin*: Point

const DefaultRadius* = 1.0

proc newShape*(name: string, x, y: float): Shape =
  Shape(name: name, origin: Point(x: x, y: y))

method describe*(s: Shape): string =
  "Shape(" & s.name & ")"

template withShape*(name: string, body: untyped) =
  let shape {.inject.} = newShape(name, 0.0, 0.0)
  body

macro logCall*(name: untyped): untyped =
  echo "calling: ", name

proc main*() =
  let a = newShape("circle", 0.0, 0.0)
  let b = newShape("square", 3.0, 4.0)
  let d = distance(a.origin, b.origin)
  let clamped = clamp(d, 0.0, 10.0)
  echo describe(a), " distance=", clamped

when isMainModule:
  main()
