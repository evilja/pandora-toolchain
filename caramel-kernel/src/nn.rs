// A small fully connected network, and what it takes to train one.
//
// Nothing here is specific to video. A `Network` is a stack of dense layers with a smooth
// activation between them and a linear last layer; it is evaluated with `forward` and trained with
// `forward_trace` / `backward` and an `Adam` step. Everything is plain `f64` loops with no
// dependency: the networks Caramel uses have a few thousand weights and a few thousand training
// scenes, which trains in seconds on one core, and a model that can be retrained by the machine
// that uses it must not need a toolchain that machine does not have.
//
// Training is deterministic: the only randomness is `Rng`, seeded by the caller.

// SplitMix64: small, fast, and good enough to shuffle scenes and draw initial weights.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_add(0x9E37_79B9_7F4A_7C15))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    // Uniform in [0, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 { 0 } else { (self.next_u64() % bound as u64) as usize }
    }

    pub fn gaussian(&mut self) -> f64 {
        let (a, b) = (self.uniform().max(1e-300), self.uniform());
        (-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            items.swap(index, self.below(index + 1));
        }
    }
}

// x * sigmoid(x): smooth, so the curves a network draws have no corners of its own making, and it
// does not saturate for large inputs the way tanh does.
fn silu(x: f64) -> f64 {
    x / (1.0 + (-x).exp())
}

fn silu_slope(x: f64) -> f64 {
    let sigmoid = 1.0 / (1.0 + (-x).exp());
    sigmoid * (1.0 + x * (1.0 - sigmoid))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub inputs: usize,
    pub outputs: usize,
    // Row by row: the weights of output 0 over every input, then of output 1.
    pub weights: Vec<f64>,
    pub bias: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Network {
    pub layers: Vec<Layer>,
}

// What one forward pass leaves behind for the backward one: every layer's input, and what each
// hidden layer computed before its activation.
#[derive(Default)]
pub struct Trace {
    inputs: Vec<Vec<f64>>,
    sums: Vec<Vec<f64>>,
    pub output: Vec<f64>,
}

// The slope of a loss with respect to every weight and bias, laid out as the network is.
#[derive(Clone)]
pub struct Gradients {
    pub weights: Vec<Vec<f64>>,
    pub bias: Vec<Vec<f64>>,
}

impl Gradients {
    pub fn clear(&mut self) {
        for layer in self.weights.iter_mut().chain(self.bias.iter_mut()) {
            layer.fill(0.0);
        }
    }
}

impl Network {
    // `sizes` is the width of every layer from the input to the output. Weights are drawn so a
    // layer keeps the scale of what it is given; the last layer starts small, so an untrained
    // network answers with its output biases.
    pub fn new(sizes: &[usize], rng: &mut Rng) -> Self {
        let count = sizes.len().saturating_sub(1);
        let layers = (0..count)
            .map(|index| {
                let (inputs, outputs) = (sizes[index], sizes[index + 1]);
                let spread = (1.0 / inputs.max(1) as f64).sqrt() * if index + 1 == count { 0.1 } else { 1.0 };
                Layer {
                    inputs,
                    outputs,
                    weights: (0..inputs * outputs).map(|_| rng.gaussian() * spread).collect(),
                    bias: vec![0.0; outputs],
                }
            })
            .collect();
        Self { layers }
    }

    pub fn inputs(&self) -> usize {
        self.layers.first().map_or(0, |layer| layer.inputs)
    }

    pub fn outputs(&self) -> usize {
        self.layers.last().map_or(0, |layer| layer.outputs)
    }

    pub fn parameters(&self) -> usize {
        self.layers.iter().map(|layer| layer.weights.len() + layer.bias.len()).sum()
    }

    pub fn gradients(&self) -> Gradients {
        Gradients {
            weights: self.layers.iter().map(|layer| vec![0.0; layer.weights.len()]).collect(),
            bias: self.layers.iter().map(|layer| vec![0.0; layer.bias.len()]).collect(),
        }
    }

    pub fn forward(&self, input: &[f64]) -> Vec<f64> {
        let mut current = input.to_vec();
        for (index, layer) in self.layers.iter().enumerate() {
            let last = index + 1 == self.layers.len();
            let mut next = layer.bias.clone();
            for (output, slot) in next.iter_mut().enumerate() {
                let row = &layer.weights[output * layer.inputs..(output + 1) * layer.inputs];
                *slot += row.iter().zip(&current).map(|(weight, value)| weight * value).sum::<f64>();
                if !last {
                    *slot = silu(*slot);
                }
            }
            current = next;
        }
        current
    }

    pub fn forward_trace(&self, input: &[f64], trace: &mut Trace) {
        trace.inputs.clear();
        trace.sums.clear();
        let mut current = input.to_vec();
        for (index, layer) in self.layers.iter().enumerate() {
            let last = index + 1 == self.layers.len();
            let mut sums = layer.bias.clone();
            for (output, slot) in sums.iter_mut().enumerate() {
                let row = &layer.weights[output * layer.inputs..(output + 1) * layer.inputs];
                *slot += row.iter().zip(&current).map(|(weight, value)| weight * value).sum::<f64>();
            }
            let next = if last { sums.clone() } else { sums.iter().map(|sum| silu(*sum)).collect() };
            trace.inputs.push(current);
            trace.sums.push(sums);
            current = next;
        }
        trace.output = current;
    }

    // Adds to `gradients` the slope of a loss whose slope with respect to the traced output is
    // `output_slope`.
    pub fn backward(&self, trace: &Trace, output_slope: &[f64], gradients: &mut Gradients) {
        let mut slope = output_slope.to_vec();
        for (index, layer) in self.layers.iter().enumerate().rev() {
            if index + 1 != self.layers.len() {
                for (value, sum) in slope.iter_mut().zip(&trace.sums[index]) {
                    *value *= silu_slope(*sum);
                }
            }
            let input = &trace.inputs[index];
            let mut below = vec![0.0; layer.inputs];
            for (output, value) in slope.iter().enumerate() {
                if *value == 0.0 {
                    continue;
                }
                gradients.bias[index][output] += value;
                let row = output * layer.inputs;
                for column in 0..layer.inputs {
                    gradients.weights[index][row + column] += value * input[column];
                    below[column] += value * layer.weights[row + column];
                }
            }
            slope = below;
        }
    }
}

// Adam with decoupled weight decay: the decay shrinks the weights (never the biases) directly, so
// how hard a weight is held towards zero does not depend on how large its gradients happen to be.
pub struct Adam {
    pub rate: f64,
    pub decay: f64,
    step: u64,
    mean: Gradients,
    variance: Gradients,
}

impl Adam {
    pub fn new(network: &Network, rate: f64, decay: f64) -> Self {
        Self { rate, decay, step: 0, mean: network.gradients(), variance: network.gradients() }
    }

    pub fn step(&mut self, network: &mut Network, gradients: &Gradients, scale: f64) {
        const BETA1: f64 = 0.9;
        const BETA2: f64 = 0.999;
        self.step += 1;
        let correction1 = 1.0 - BETA1.powi(self.step as i32);
        let correction2 = 1.0 - BETA2.powi(self.step as i32);
        let rate = self.rate;
        let update = |values: &mut [f64], slopes: &[f64], mean: &mut [f64], variance: &mut [f64], decay: f64| {
            for index in 0..values.len() {
                let slope = slopes[index] * scale;
                mean[index] = BETA1 * mean[index] + (1.0 - BETA1) * slope;
                variance[index] = BETA2 * variance[index] + (1.0 - BETA2) * slope * slope;
                let step = (mean[index] / correction1) / ((variance[index] / correction2).sqrt() + 1e-8);
                values[index] -= rate * (step + decay * values[index]);
            }
        };
        for (index, layer) in network.layers.iter_mut().enumerate() {
            update(&mut layer.weights, &gradients.weights[index], &mut self.mean.weights[index], &mut self.variance.weights[index], self.decay);
            update(&mut layer.bias, &gradients.bias[index], &mut self.mean.bias[index], &mut self.variance.bias[index], 0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generator_is_reproducible_and_spread_out() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        let draws: Vec<f64> = (0..2000).map(|_| a.uniform()).collect();
        assert!(draws.iter().all(|value| (0.0..1.0).contains(value)));
        assert!((draws.iter().sum::<f64>() / 2000.0 - 0.5).abs() < 0.03);
        assert_eq!((0..5).map(|_| b.uniform()).collect::<Vec<_>>(), draws[..5]);
        let normal: Vec<f64> = (0..4000).map(|_| a.gaussian()).collect();
        let mean = normal.iter().sum::<f64>() / 4000.0;
        let variance = normal.iter().map(|value| (value - mean).powi(2)).sum::<f64>() / 4000.0;
        assert!(mean.abs() < 0.06 && (variance - 1.0).abs() < 0.1, "{mean} {variance}");
        let mut order: Vec<usize> = (0..50).collect();
        a.shuffle(&mut order);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert!(order != sorted && sorted == (0..50).collect::<Vec<_>>());
    }

    // The backward pass against the slope measured by nudging each weight.
    #[test]
    fn gradients_match_finite_differences() {
        let mut rng = Rng::new(11);
        let mut network = Network::new(&[3, 5, 4, 2], &mut rng);
        for layer in &mut network.layers {
            for bias in &mut layer.bias {
                *bias = rng.gaussian() * 0.3;
            }
        }
        let input = [0.4, -1.2, 0.7];
        let target = [0.3, -0.5];
        let loss = |network: &Network| -> f64 {
            network.forward(&input).iter().zip(target).map(|(out, want)| 0.5 * (out - want).powi(2)).sum()
        };
        let mut trace = Trace::default();
        network.forward_trace(&input, &mut trace);
        assert_eq!(trace.output, network.forward(&input));
        let slope: Vec<f64> = trace.output.iter().zip(target).map(|(out, want)| out - want).collect();
        let mut gradients = network.gradients();
        network.backward(&trace, &slope, &mut gradients);
        for layer in 0..network.layers.len() {
            for index in 0..network.layers[layer].weights.len() {
                let kept = network.layers[layer].weights[index];
                network.layers[layer].weights[index] = kept + 1e-6;
                let up = loss(&network);
                network.layers[layer].weights[index] = kept - 1e-6;
                let down = loss(&network);
                network.layers[layer].weights[index] = kept;
                let measured = (up - down) / 2e-6;
                assert!((measured - gradients.weights[layer][index]).abs() < 1e-6, "layer {layer} weight {index}: {measured} vs {}", gradients.weights[layer][index]);
            }
            for index in 0..network.layers[layer].bias.len() {
                let kept = network.layers[layer].bias[index];
                network.layers[layer].bias[index] = kept + 1e-6;
                let up = loss(&network);
                network.layers[layer].bias[index] = kept - 1e-6;
                let down = loss(&network);
                network.layers[layer].bias[index] = kept;
                assert!(((up - down) / 2e-6 - gradients.bias[layer][index]).abs() < 1e-6);
            }
        }
    }

    // A function no single linear layer can draw: the product of two inputs.
    #[test]
    fn a_network_learns_what_a_line_cannot() {
        let mut rng = Rng::new(3);
        let mut network = Network::new(&[2, 12, 12, 1], &mut rng);
        let mut adam = Adam::new(&network, 0.01, 0.0);
        let samples: Vec<([f64; 2], f64)> = (0..200)
            .map(|_| {
                let (a, b) = (rng.uniform() * 2.0 - 1.0, rng.uniform() * 2.0 - 1.0);
                ([a, b], a * b)
            })
            .collect();
        let error = |network: &Network| -> f64 {
            samples.iter().map(|(input, want)| (network.forward(input)[0] - want).powi(2)).sum::<f64>() / samples.len() as f64
        };
        let before = error(&network);
        let mut trace = Trace::default();
        let mut gradients = network.gradients();
        for _ in 0..400 {
            gradients.clear();
            for (input, want) in &samples {
                network.forward_trace(input, &mut trace);
                network.backward(&trace, &[trace.output[0] - want], &mut gradients);
            }
            adam.step(&mut network, &gradients, 1.0 / samples.len() as f64);
        }
        let after = error(&network);
        assert!(before > 0.05 && after < 0.002, "{before} -> {after}");
    }

    #[test]
    fn decay_holds_weights_and_leaves_biases() {
        let mut rng = Rng::new(5);
        let mut network = Network::new(&[2, 3, 1], &mut rng);
        network.layers[0].bias[0] = 2.0;
        let weight = network.layers[0].weights[0];
        let mut adam = Adam::new(&network, 0.1, 0.5);
        let zero = network.gradients();
        adam.step(&mut network, &zero, 1.0);
        assert!((network.layers[0].weights[0] - weight * 0.95).abs() < 1e-12);
        assert_eq!(network.layers[0].bias[0], 2.0);
        assert_eq!((network.inputs(), network.outputs(), network.parameters()), (2, 1, 13));
    }
}
