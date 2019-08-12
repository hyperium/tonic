pub trait Codec {
    type Encode;
    type Decode;

    type Encoder;
}
