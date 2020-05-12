//! Implementation verifiable encryption of discrete logarithms with class
//! groups with CL-DL proof rom https://eprint.iacr.org/2020/084.pdf Figure 7.
use super::ErrorReason;
use crate::bn_to_gen;
use crate::curv::arithmetic::traits::Modulo;
use crate::curv::cryptographic_primitives::hashing::traits::Hash;
use crate::isprime;
use crate::pari_init;
use crate::primitives::numerical_log;
use crate::primitives::prng;
use crate::BinaryQF;
use curv::arithmetic::traits::Samplable;
use curv::cryptographic_primitives::hashing::hash_sha256::HSha256;
use curv::elliptic::curves::traits::{ECPoint, ECScalar};
use curv::BigInt;
use curv::{FE, GE};
use paillier::keygen;
use std::os::raw::c_int;

const SECURITY_PARAMETER: usize = 128;
const C: usize = 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClassGroup {
    pub delta_k: BigInt,
    pub delta_q: BigInt,
    pub gq: BinaryQF,
    pub stilde: BigInt,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Ciphertext {
    pub c1: BinaryQF,
    pub c2: BinaryQF,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TTriplets {
    pub t1: BinaryQF,
    pub t2: BinaryQF,
    pub T: GE,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct U1U2 {
    pub u1: BigInt,
    pub u2: BigInt,
}

#[derive(Debug, Clone)]
pub struct ProofError;

impl ClassGroup {
    pub fn new(lam: &usize) -> Self {
        unsafe { pari_init(10000000, 2) };
        let q = &FE::q();
        let mu = q.bit_length();
        assert!(lam > &(mu + 2));
        let k = lam - mu;
        let two = BigInt::from(2);
        let mut r = BigInt::sample_range(
            &two.pow((k - 1) as u32),
            &(two.pow(k as u32) - BigInt::one()),
        );

        let mut qtilde = next_probable_prime(&r);

        while (q * &qtilde).mod_floor(&BigInt::from(4)) != BigInt::from(3)
            || jacobi(q, &qtilde).unwrap() != -1
        {
            r = BigInt::sample_range(
                &two.pow((k - 1) as u32),
                &(two.pow(k as u32) - BigInt::one()),
            );
            qtilde = next_probable_prime(&r);
        }

        assert!(&(BigInt::from(4) * q) < &qtilde);

        let delta_k = -q * &qtilde;
        let delta_q = &delta_k * q.pow(2);

        let delta_k_abs: BigInt = -delta_k.clone();
        let log_delta_k_abs = numerical_log(&delta_k_abs);
        let delta_k_abs_sqrt = delta_k_abs.sqrt();
        let stilde = log_delta_k_abs * delta_k_abs_sqrt;

        let mut r = BigInt::from(3);
        while jacobi(&delta_k, &r).unwrap() != 1 {
            r = next_probable_prime(&r)
        }

        let rgoth = BinaryQF::primeform(&delta_k, &r);

        let rgoth_square = rgoth.compose(&rgoth).reduce();

        let gq_tmp = rgoth_square.phi_q_to_the_minus_1(&q).reduce();

        let gq = gq_tmp.exp(&q);

        ClassGroup {
            stilde,
            delta_k,
            delta_q,
            gq,
        }
    }

    pub fn new_from_setup(lam: &usize, seed: &BigInt) -> Self {
        let q = &FE::q();
        unsafe { pari_init(100000000, 2) };
        let mu = q.bit_length();
        debug_assert!(lam > &(mu + 2));
        let k = lam - mu;
        let two = BigInt::from(2);
        let mut r = BigInt::sample_range(
            &two.pow((k - 1) as u32),
            &(two.pow(k as u32) - BigInt::one()),
        );

        let mut qtilde = next_probable_prime(&r);

        while (q * &qtilde).mod_floor(&BigInt::from(4)) != BigInt::from(3)
            || jacobi(q, &qtilde).unwrap() != -1
        {
            r = BigInt::sample_range(
                &two.pow((k - 1) as u32),
                &(two.pow(k as u32) - BigInt::one()),
            );
            qtilde = next_probable_prime(&r);
        }

        debug_assert!(&(BigInt::from(4) * q) < &qtilde);

        let delta_k = -q * &qtilde;
        let delta_q = &delta_k * q.pow(2);

        let delta_k_abs: BigInt = -delta_k.clone();
        let log_delta_k_abs = numerical_log(&delta_k_abs);
        let delta_k_abs_sqrt = delta_k_abs.sqrt();
        let stilde = log_delta_k_abs * delta_k_abs_sqrt;

        // Assuming GRH the prime forms f_p with p<=6 ln^2(|delta_k|) generate the class group cf.
        // Cohen course comp. algebraic. number theory 5.5.1.
        // In practice we take only ln(-deltak)/ln(ln(-deltak))  primes and exponents up to 20 (cf. 5.5.2)
        // But as in https://eprint.iacr.org/2018/705.pdf page 20 we need pairwise coprime exponents
        // for the strong root assumption to hold so we take greater exponents to ensure that,
        // say up to 15 bits. (in fact for our purposes exponents globally coprime might be sufficient instead of pairwise coprimes)
        let mut prime_forms_vec: Vec<BinaryQF> = Vec::new();
        let mut r = BigInt::from(3);
        let ln_delta_k = numerical_log(&(-&delta_k));

        let num_of_prime_forms = ln_delta_k.div_floor(&numerical_log(&ln_delta_k));

        let mut i = BigInt::zero();
        while i < num_of_prime_forms {
            while jacobi(&delta_k, &r).unwrap() != 1 {
                r = next_probable_small_prime(&r)
            }
            prime_forms_vec.push(BinaryQF::primeform(&delta_k, &r));
            r = next_probable_small_prime(&r);
            i = i + 1;
        }
        let mut rgoth = BinaryQF::binary_quadratic_form_principal(&delta_k);

        //pseudo random element of class group Cl(delta_k) : prod f_p^e_p, with pairwise coprime exponents
        // generate enough pseudo randomness : 15 bits per exponents e_p

        // find exponent
        let mut i = 0;
        let mut rand_bits_i: BigInt;
        let mut prod_exponent = BigInt::one();
        while i < prime_forms_vec.len() {
            // extract 15bits
            rand_bits_i = prng(seed, i.clone(), 15);
            while rand_bits_i.gcd(&prod_exponent) != BigInt::one() {
                rand_bits_i = rand_bits_i + 1;
            }
            rgoth = rgoth
                .compose(&prime_forms_vec[i].exp(&rand_bits_i))
                .reduce();
            prod_exponent = prod_exponent * &rand_bits_i;
            i = i + 1;
        }

        let rgoth_square = rgoth.compose(&rgoth).reduce();

        let gq_tmp = rgoth_square.phi_q_to_the_minus_1(&q).reduce();

        let gq = gq_tmp.exp(&q);

        ClassGroup {
            delta_k,
            delta_q,
            gq,
            stilde,
        }
    }

    //repeat random element g_q generation using seed and delta_k
    pub fn setup_verify(&self, seed: &BigInt) -> Result<(), ErrorReason> {
        unsafe { pari_init(100000000, 2) };

        let mut prime_forms_vec: Vec<BinaryQF> = Vec::new();
        let ln_delta_k = numerical_log(&(-&self.delta_k));
        let num_of_prime_forms = ln_delta_k.div_floor(&numerical_log(&ln_delta_k));

        let mut r = BigInt::from(3);
        let mut i = BigInt::zero();
        while i < num_of_prime_forms {
            while jacobi(&self.delta_k, &r).unwrap() != 1 {
                r = next_probable_small_prime(&r)
            }
            prime_forms_vec.push(BinaryQF::primeform(&self.delta_k, &r));
            r = next_probable_small_prime(&r);
            i = i + 1;
        }

        let mut rgoth = BinaryQF::binary_quadratic_form_principal(&self.delta_k);

        //pseudo random element of class group Cl(delta_k) : prod f_p^e_p, with pairwise coprime exponents
        // generate enough pseudo randomness : 15 bits per exponents e_p

        // find exponent
        let mut i = 0;
        let mut rand_bits_i: BigInt;
        let mut prod_exponent = BigInt::one();
        while i < prime_forms_vec.len() {
            // extract 15bits
            rand_bits_i = prng(seed, i.clone(), 15);
            while rand_bits_i.gcd(&prod_exponent) != BigInt::one() {
                rand_bits_i = rand_bits_i + 1;
            }
            rgoth = rgoth
                .compose(&prime_forms_vec[i].exp(&rand_bits_i))
                .reduce();
            prod_exponent = prod_exponent * &rand_bits_i;
            i = i + 1;
        }

        let rgoth_square = rgoth.compose(&rgoth).reduce();

        let gq_tmp = rgoth_square.phi_q_to_the_minus_1(&FE::q()).reduce();

        let gq = gq_tmp.exp(&FE::q());
        match gq == self.gq {
            true => Ok(()),
            false => Err(ErrorReason::SetupError),
        }
    }

    /// randomly sample a scalar and compute its corresponding group element by multiplying g_q
    pub fn sample_gq_pair(&self) -> (BigInt, BinaryQF) {
        let scalar = BigInt::sample_below(&(&self.stilde * BigInt::from(2).pow(40)));
        let group_element = self.gq.exp(&scalar);
        (scalar, group_element)
    }
}

// copied from https://docs.rs/crate/quadratic/0.3.1/source/src/lib.rs
// changed to support BigInt
// TODO: put in utility module, expend to Kronecker
pub fn jacobi(a: &BigInt, n: &BigInt) -> Option<i8> {
    let zero = BigInt::zero();
    // jacobi symbol is only defined for odd positive moduli
    if n.mod_floor(&BigInt::from(2)) == zero || n <= &BigInt::zero() {
        return None;
    }

    // Raise a mod n, then start the unsigned algorithm
    let mut acc = 1;
    let mut num = a.mod_floor(&n);
    let mut den = n.clone();
    loop {
        // reduce numerator
        num = num.mod_floor(&den);
        if num == zero {
            return Some(0);
        }

        // extract factors of two from numerator
        while num.mod_floor(&BigInt::from(2)) == zero {
            acc *= two_over(&den);
            num = num.div_floor(&BigInt::from(2));
        }
        // if numerator is 1 => this sub-symbol is 1
        if num == BigInt::one() {
            return Some(acc);
        }
        // shared factors => one sub-symbol is zero
        if num.gcd(&den) > BigInt::one() {
            return Some(0);
        }
        // num and den are now odd co-prime, use reciprocity law:
        acc *= reciprocity(&num, &den);
        let tmp = num;
        num = den.clone();
        den = tmp;
    }
}

fn two_over(n: &BigInt) -> i8 {
    if n.mod_floor(&BigInt::from(8)) == BigInt::one()
        || n.mod_floor(&BigInt::from(8)) == BigInt::from(7)
    {
        1
    } else {
        -1
    }
}

fn reciprocity(num: &BigInt, den: &BigInt) -> i8 {
    if num.mod_floor(&BigInt::from(4)) == BigInt::from(3)
        && den.mod_floor(&BigInt::from(4)) == BigInt::from(3)
    {
        -1
    } else {
        1
    }
}

fn next_probable_prime(r: &BigInt) -> BigInt {
    let one = BigInt::from(1);
    let mut qtilde = r + &one;
    while !keygen::is_prime(&qtilde) {
        qtilde = qtilde + &one;
    }
    qtilde
}

// used for testing small primes where our prime test fails. We use Pari isprime which provides
// determinstic perfect primality checking.
fn next_probable_small_prime(r: &BigInt) -> BigInt {
    let one = BigInt::from(1);
    let mut qtilde = r + &one;
    let mut qtilde_gen = bn_to_gen(&(r + &one));
    unsafe {
        while isprime(qtilde_gen) as c_int != 1 {
            qtilde = qtilde + &one;
            qtilde_gen = bn_to_gen(&qtilde);
        }
    }
    qtilde
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// The public key with its secret key which is chosen in the range of the
/// upper bound of g_q * 2^40 (to prevent modular reduction bias).
pub struct KeyPair {
    pub public_key: BinaryQF,
    pub secret_key: BigInt,
}

impl KeyPair {
    pub fn random(group: &ClassGroup) -> Self {
        let (secret_key, public_key) = group.sample_gq_pair();

        KeyPair {
            secret_key,
            public_key,
        }
    }
}

/// ElGamal encrypts the message under the public key.
///
/// Returns the secret randomness used.
pub fn encrypt(group: &ClassGroup, public_key: &BinaryQF, m: &FE) -> (Ciphertext, BigInt) {
    unsafe { pari_init(10000000, 2) };
    let (r, R) = group.sample_gq_pair();
    let exp_f = BinaryQF::expo_f(&FE::q(), &group.delta_q, &m.to_big_int());
    let h_exp_r = public_key.exp(&r);

    (
        Ciphertext {
            c1: R,
            c2: h_exp_r.compose(&exp_f).reduce(),
        },
        r,
    )
}

pub fn verifiably_encrypt(
    group: &ClassGroup,
    public_key: &BinaryQF,
    DL_pair: (&FE, &GE),
) -> (Ciphertext, CLDLProof) {
    let (x, X) = DL_pair;
    let (ciphertext, r) = encrypt(group, public_key, x);

    let proof = CLDLProof::prove(group, (x, r), (public_key, &ciphertext, X));
    (ciphertext, proof)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CLDLProof {
    t_triple: TTriplets,
    u1u2: U1U2,
}

impl CLDLProof {
    fn prove(
        group: &ClassGroup,
        witness: (&FE, BigInt),
        statement: (&BinaryQF, &Ciphertext, &GE),
    ) -> Self {
        unsafe { pari_init(10000000, 2) };
        let (x, r) = witness;
        let (public_key, ciphertext, X) = statement;

        let r1 = BigInt::sample_below(
            &(&group.stilde
                * BigInt::from(2).pow(40)
                * BigInt::from(2).pow(SECURITY_PARAMETER as u32)
                * BigInt::from(2).pow(40)),
        );
        let r2_fe: FE = FE::new_random();
        let r2 = r2_fe.to_big_int();
        let fr2 = BinaryQF::expo_f(&FE::q(), &group.delta_q, &r2);
        let pkr1 = public_key.exp(&r1);
        let t2 = fr2.compose(&pkr1).reduce();
        let T = GE::generator() * r2_fe;
        let t1 = group.gq.exp(&r1);
        let t_triple = TTriplets { t1, t2, T };

        let k = Self::challenge(public_key, &t_triple, ciphertext, X);

        let u1 = r1 + &k * &r;
        let u2 = BigInt::mod_add(&r2, &(&k * x.to_big_int()), &FE::q());
        let u1u2 = U1U2 { u1, u2 };

        Self { t_triple, u1u2 }
    }

    /// Compute the Fiat-Shamir challenge for the proof.
    fn challenge(public_key: &BinaryQF, t: &TTriplets, ciphertext: &Ciphertext, X: &GE) -> BigInt {
        use crate::curv::arithmetic::traits::Converter;
        let hash256 = HSha256::create_hash(&[
            // hash the statement i.e. the discrete log of Q is encrypted in (c1,c2) under encryption key h.
            &X.bytes_compressed_to_big_int(),
            &BigInt::from(ciphertext.c1.to_bytes().as_ref()),
            &BigInt::from(ciphertext.c2.to_bytes().as_ref()),
            &BigInt::from(public_key.to_bytes().as_ref()),
            // hash Sigma protocol commitments
            &BigInt::from(t.t1.to_bytes().as_ref()),
            &BigInt::from(t.t2.to_bytes().as_ref()),
            &t.T.bytes_compressed_to_big_int(),
        ]);

        let hash128 = &BigInt::to_vec(&hash256)[..SECURITY_PARAMETER / 8];
        BigInt::from(hash128)
    }

    pub fn verify(
        &self,
        group: &ClassGroup,
        public_key: &BinaryQF,
        ciphertext: &Ciphertext,
        X: &GE,
    ) -> Result<(), ProofError> {
        unsafe { pari_init(10000000, 2) };
        let mut flag = true;

        // reconstruct k
        let k = Self::challenge(public_key, &self.t_triple, ciphertext, X);

        let sample_size = &group.stilde
            * (BigInt::from(2).pow(40))
            * BigInt::from(2).pow(SECURITY_PARAMETER as u32)
            * (BigInt::from(2).pow(40) + BigInt::one());

        //length test u1:
        if &self.u1u2.u1 > &sample_size || &self.u1u2.u1 < &BigInt::zero() {
            flag = false;
        }
        // length test u2:
        if &self.u1u2.u2 > &FE::q() || &self.u1u2.u2 < &BigInt::zero() {
            flag = false;
        }

        let c1k = ciphertext.c1.exp(&k);
        let t1c1k = self.t_triple.t1.compose(&c1k).reduce();
        let gqu1 = group.gq.exp(&&self.u1u2.u1);
        if t1c1k != gqu1 {
            flag = false;
        };

        let k_bias_fe: FE = ECScalar::from(&(k.clone() + BigInt::one()));
        let g = GE::generator();
        let t2kq = (self.t_triple.T + X * &k_bias_fe).sub_point(&X.get_element());
        let u2p = &g * &ECScalar::from(&self.u1u2.u2);
        if t2kq != u2p {
            flag = false;
        }

        let pku1 = public_key.exp(&self.u1u2.u1);
        let fu2 = BinaryQF::expo_f(&FE::q(), &group.delta_q, &self.u1u2.u2);
        let c2k = ciphertext.c2.exp(&k);
        let t2c2k = self.t_triple.t2.compose(&c2k).reduce();
        let pku1fu2 = pku1.compose(&fu2).reduce();
        if t2c2k != pku1fu2 {
            flag = false;
        }
        match flag {
            true => Ok(()),
            false => Err(ProofError),
        }
    }
}

pub fn decrypt(group: &ClassGroup, secret_key: &BigInt, c: &Ciphertext) -> FE {
    unsafe { pari_init(10000000, 2) };
    let c1_x = c.c1.exp(&secret_key);
    let c1_x_inv = c1_x.inverse();
    let tmp = c.c2.compose(&c1_x_inv).reduce();
    let plaintext = BinaryQF::discrete_log_f(&FE::q(), &group.delta_q, &tmp);
    debug_assert!(plaintext < FE::q());
    ECScalar::from(&plaintext)
}

/// Multiplies the encrypted value by `val`.
//TODO: add unit test
pub fn eval_scal(c: &Ciphertext, val: &BigInt) -> Ciphertext {
    unsafe { pari_init(10000000, 2) };
    let c_new = Ciphertext {
        c1: c.c1.exp(&val),
        c2: c.c2.exp(&val),
    };
    c_new
}

/// Homomorphically adds two ciphertexts so that the resulting ciphertext is the sum of the two input ciphertexts
pub fn eval_sum(c1: &Ciphertext, c2: &Ciphertext) -> Ciphertext {
    unsafe { pari_init(10000000, 2) };
    let c_new = Ciphertext {
        c1: c1.c1.compose(&c2.c1).reduce(),
        c2: c1.c2.compose(&c2.c2).reduce(),
    };
    c_new
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn encrypt_and_decrypt() {
        let group = ClassGroup::new(&1600);
        let keypair = KeyPair::random(&group);
        let message = FE::new_random();
        let (ciphertext, _) = encrypt(&group, &keypair.public_key, &message);
        let plaintext = decrypt(&group, &keypair.secret_key, &ciphertext);
        assert_eq!(plaintext, message);
    }

    #[test]
    fn compute_discrete_log_in_DLEasy_subgroup() {
        let group = ClassGroup::new(&1600);
        let m = BigInt::from(10000);
        let exp_f = BinaryQF::expo_f(&FE::q(), &group.delta_q, &m);
        let m_tag = BinaryQF::discrete_log_f(&FE::q(), &group.delta_q, &exp_f);
        assert_eq!(m, m_tag);
    }

    #[test]
    fn verifiably_encrypt_verify_and_decrypt() {
        // digits of pi
        let seed = str::parse(
            "314159265358979323846264338327950288419716939937510582097494459230781640628620899862803482534211706798214808651328230664709384460955058223172535940812848"
        ).unwrap();
        let group = ClassGroup::new_from_setup(&1600, &seed);
        let keypair = KeyPair::random(&group);
        let dl_keypair = {
            let sk = FE::new_random();
            let pk = GE::generator() * sk;
            (sk, pk)
        };
        let (ciphertext, proof) =
            verifiably_encrypt(&group, &keypair.public_key, (&dl_keypair.0, &dl_keypair.1));

        let wrong_dl_pk = dl_keypair.1 + &GE::generator();

        assert!(
            proof
                .verify(&group, &keypair.public_key, &ciphertext, &dl_keypair.1)
                .is_ok(),
            "proof is valid against valid DL key"
        );

        assert!(
            proof
                .verify(&group, &keypair.public_key, &ciphertext, &wrong_dl_pk)
                .is_err(),
            "proof is invalid against invalid DL key"
        );

        assert_eq!(
            decrypt(&group, &keypair.secret_key, &ciphertext),
            dl_keypair.0,
            "plaintext matches what was encrypted"
        );
    }

    #[test]
    fn cl_dl_test_setup() {
        // digits of pi
        let seed = str::parse(
            "314159265358979323846264338327950288419716939937510582097494459230781640628620899862803482534211706798214808651328230664709384460955058223172535940812848"
        ).unwrap();

        let group = ClassGroup::new_from_setup(&1600, &seed);
        assert!(group.setup_verify(&seed).is_ok());
    }
}
