# Vyper Sample Project

Minimal ERC20-like Vyper contract for ecp-analyzer fixture testing.

## Expected symbols

| Name | Kind | Notes |
|---|---|---|
| `__init__` | Function | constructor, `@external` decorator |
| `balanceOf` | Function | `@external @view` decorators |
| `transfer` | Function | `@external` decorator |
| `_mint` | Function | `@internal` decorator |
| `DECIMALS` | Const | named constant |
| `name` | Const | state variable |
| `symbol` | Const | state variable |
| `totalSupply` | Const | state variable |
| `balances` | Const | state variable |

## Import

`import ERC20Interface` is captured as an import with source `ERC20Interface`.
