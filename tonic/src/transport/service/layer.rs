use tower::{
    layer::{Layer, Stack},
    util::Either,
    ServiceBuilder,
};

pub(crate) trait ServiceBuilderExt<L> {
    fn layer_fn<F: Fn(S) -> Out, S, Out>(self, f: F) -> ServiceBuilder<Stack<LayerFn<F>, L>>;

    fn optional_layer_fn<F: Fn(S) -> Out, S, Out>(
        self,
        f: Option<F>,
    ) -> ServiceBuilder<Stack<OptionalLayer<LayerFn<F>>, L>>;

    fn optional_layer<T>(self, l: Option<T>) -> ServiceBuilder<Stack<OptionalLayer<T>, L>>;
}

impl<L> ServiceBuilderExt<L> for ServiceBuilder<L> {
    fn layer_fn<F, S, Out>(self, f: F) -> ServiceBuilder<Stack<LayerFn<F>, L>>
    where
        F: Fn(S) -> Out,
    {
        self.layer(LayerFn(f))
    }

    fn optional_layer_fn<F, S, Out>(
        self,
        f: Option<F>,
    ) -> ServiceBuilder<Stack<OptionalLayer<LayerFn<F>>, L>>
    where
        F: Fn(S) -> Out,
    {
        let layer = OptionalLayer {
            inner: f.map(LayerFn),
        };

        self.layer(layer)
    }

    fn optional_layer<T>(self, inner: Option<T>) -> ServiceBuilder<Stack<OptionalLayer<T>, L>> {
        self.layer(OptionalLayer { inner })
    }
}

// TODO: figure out why this is causing a warning even though its used in optional_layer_fn
#[allow(dead_code)]
pub(crate) fn layer_fn<F>(f: F) -> LayerFn<F> {
    LayerFn(f)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LayerFn<F>(F);

impl<F, S, Out> Layer<S> for LayerFn<F>
where
    F: Fn(S) -> Out,
{
    type Service = Out;

    fn layer(&self, inner: S) -> Self::Service {
        (self.0)(inner)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OptionalLayer<L> {
    inner: Option<L>,
}

impl<S, L> Layer<S> for OptionalLayer<L>
where
    L: Layer<S>,
{
    type Service = Either<L::Service, S>;

    fn layer(&self, s: S) -> Self::Service {
        if let Some(inner) = &self.inner {
            Either::A(inner.layer(s))
        } else {
            Either::B(s)
        }
    }
}
