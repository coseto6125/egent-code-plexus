# SPDX-License-Identifier: MIT
# Simple ERC20-like token in Vyper

import ERC20Interface

# Named constant
DECIMALS: constant(uint256) = 18
MAX_SUPPLY: constant(uint256) = 1000000

# State variables (as assignments)
totalSupply: uint256 = 0
balances: HashMap[address, uint256] = {}

@external
def __init__(_supply: uint256):
    self.totalSupply = _supply

@external
@view
def balanceOf(_owner: address) -> uint256:
    return self.balances[_owner]

@external
def transfer(_to: address, _value: uint256) -> bool:
    assert self.balances[msg.sender] >= _value
    return True

@internal
def _mint(_to: address, _value: uint256):
    self.totalSupply += _value
