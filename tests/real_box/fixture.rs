//! The tests' own [`Fixture`]: its BoxLite home shares the images the run has downloaded
//! (jonaswre/boxlite-quick#44), unless it is cold.

use super::*;

/// The references of the images in `f`'s BoxLite home, as BoxLite lists them.
async fn images(f: &Fixture) -> Vec<String> {
    let rt = f.runtime();
    let images = rt.images().unwrap().list().await.unwrap();
    images.into_iter().map(|i| i.reference).collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "pulls images; needs internet access"]
async fn a_fixtures_home_starts_with_the_small_images_the_tests_use() {
    let f = Fixture::new();

    let images = images(&f).await;

    for image in [IMAGE, GIT_IMAGE, NON_ROOT_IMAGE] {
        assert!(
            images.iter().any(|i| i == image),
            "no {image} in {images:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "opens a BoxLite runtime"]
async fn a_cold_fixtures_home_starts_with_no_images() {
    let f = Fixture::cold();

    assert_eq!(images(&f).await, Vec::<String>::new());
}
