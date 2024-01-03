use lightning::sign::EntropySource;

pub struct TestEntropy {}
impl EntropySource for TestEntropy {
	fn get_secure_random_bytes(&self) -> [u8; 32] {
		[0; 32]
	}
}
