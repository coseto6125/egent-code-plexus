use starknet::ContractAddress;

mod token {
    use core::traits::Into;

    struct TokenInfo {
        name: felt252,
        symbol: felt252,
        decimals: u8,
    }

    trait IERC20 {
        fn total_supply(self: @ContractState) -> u256;
        fn balance_of(self: @ContractState, account: ContractAddress) -> u256;
        fn transfer(ref self: ContractState, recipient: ContractAddress, amount: u256) -> bool;
    }

    impl ERC20Impl of IERC20 {
        fn total_supply(self: @ContractState) -> u256 {
            1000000_u256
        }

        fn balance_of(self: @ContractState, account: ContractAddress) -> u256 {
            0_u256
        }

        fn transfer(ref self: ContractState, recipient: ContractAddress, amount: u256) -> bool {
            true
        }
    }

    fn create_token(name: felt252, symbol: felt252) -> TokenInfo {
        TokenInfo { name, symbol, decimals: 18_u8 }
    }
}
