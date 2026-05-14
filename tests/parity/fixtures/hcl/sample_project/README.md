# HCL / Terraform sample fixture

Minimal but realistic Terraform configuration exercising all five HCL block kinds.

## Expected symbols

| Name | Kind | Notes |
|------|------|-------|
| `aws_s3_bucket.app_artifacts` | Class | `resource` block, two-label name |
| `aws_s3_bucket_versioning.app_artifacts` | Class | `resource` block |
| `aws_ami.ubuntu` | Class | `data` block, two-label name |
| `aws_caller_identity.current` | Class | `data` block |
| `vpc` | Class | `module` block; `source = "./modules/vpc"` → import edge |
| `region` | Const | `variable` block |
| `env` | Const | `variable` block |
| `bucket_name` | Const | `output` block |
| `vpc_id` | Const | `output` block |
| `common_tags` | Const | `locals` attribute |
