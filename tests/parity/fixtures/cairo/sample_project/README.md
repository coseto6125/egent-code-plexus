# Cairo Sample Project

Minimal Cairo 1.0 fixture for cgn-analyzer parser verification.

## Symbols expected

- `TokenInfo` — struct
- `IERC20` — trait (captured as class)
- `ERC20` — impl (captured as class, with heritage `IERC20`)
- `token` — module (captured as class)
- `create_token` — free function
- `total_supply`, `balance_of`, `transfer` — functions inside impl/trait
- `starknet::ContractAddress` — import

## Query to verify

```
cgn-cli context --repo tests/parity/fixtures/cairo/sample_project --name create_token
```
