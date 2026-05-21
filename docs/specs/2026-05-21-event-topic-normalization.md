# Event Topic Canonicalization

Locked normalization rules for event topic identifiers (e.g., Kafka topics, event bus channel names, webhook event types).

## Normalization Rules (in order)

1. **Strip environment prefixes**: Remove `prod.`, `dev.`, `staging.`, `<env>.` from the start
2. **Strip version suffix**: Remove `.v[0-9]+` from the end
3. **Lowercase**: Convert to lowercase
4. **Normalize separators**: Replace any of `.`, `_`, `-`, `:`, `/` with `/`
5. **Trim slashes**: Remove leading and trailing `/`
6. **CamelCase to snake_case**: Convert each segment separated by `/` from CamelCase to snake_case

## Result

The output is a canonical form suitable for deduplication and cross-system matching. Separators are normalized to `/` and casing is lowercased.

## Examples

| Input | Output | Notes |
|-------|--------|-------|
| `prod.order.created` | `order/created` | Env prefix stripped |
| `dev.order-created.v1` | `order/created` | Env prefix + version stripped |
| `OrderCreated` | `order/created` | CamelCase normalized |
| `userSignedUp` | `user/signed/up` | Multi-word CamelCase |
| `order_created` | `order/created` | Underscore → slash |
| `order-created` | `order/created` | Hyphen → slash |
| `order.created` | `order/created` | Dot → slash |
| `order:created` | `order/created` | Colon → slash |
| `order/created` | `order/created` | Already canonical |
| `eu-west-1.order.created` | `eu-west-1/order/created` | Region prefix (not env) preserved |
| `tenant-123.order.created` | `tenant-123/order/created` | Tenant ID preserved |

## Negative Documentation

- **Hyphen and slash collapse intentionally**: `order-created` and `order/created` both normalize to `order/created`. Systems using different separators are expected to normalize before comparison.

- **Region prefixes are preserved**: `eu-west-1.order.created` → `eu-west-1/order/created` and `eu-west-2.order.created` → `eu-west-2/order/created` remain distinct. Only environment prefixes (`prod.`, `dev.`, `staging.`) are stripped.

- **Tenant identifiers are preserved**: `tenant-123.order.created` and `tenant-456.order.created` normalize to `tenant-123/order/created` and `tenant-456/order/created` respectively, staying distinct.
