use std::borrow::Borrow;
use std::ops::Deref;
#[cfg(feature = "single-threaded")]
use std::rc::Rc;
#[cfg(not(feature = "single-threaded"))]
use std::sync::Arc;

#[cfg(feature = "single-threaded")]
type Inner = Rc<[u8]>;
#[cfg(not(feature = "single-threaded"))]
type Inner = Arc<[u8]>;

/// An immutable, cheaply cloneable byte buffer.
///
/// By default, `SharedBytes` uses atomic reference counting and can be sent
/// and shared between threads. Enable the `single-threaded` feature to use
/// non-atomic reference counting instead. In that configuration,
/// `SharedBytes` is neither [`Send`] nor [`Sync`].
///
/// The selected reference-counting implementation is an implementation detail;
/// callers should use [`AsRef<[u8]>`], [`Deref`], and [`Clone`] to access and
/// share the data.
#[repr(transparent)]
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SharedBytes(Inner);

impl SharedBytes {
    /// Creates a `SharedBytes` containing a copy of `bytes`.
    pub fn copy_from_slice(bytes: &[u8]) -> Self {
        Self(bytes.into())
    }

    /// Returns the bytes as a slice.
    pub fn as_slice(&self) -> &[u8] {
        self.0.as_ref()
    }

    /// Returns the number of bytes in this buffer.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if this buffer contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Borrow<[u8]> for SharedBytes {
    fn borrow(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Deref for SharedBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl From<Vec<u8>> for SharedBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes.into_boxed_slice().into())
    }
}

impl From<Box<[u8]>> for SharedBytes {
    fn from(bytes: Box<[u8]>) -> Self {
        Self(bytes.into())
    }
}

impl From<&[u8]> for SharedBytes {
    fn from(bytes: &[u8]) -> Self {
        Self::copy_from_slice(bytes)
    }
}

impl<const N: usize> From<[u8; N]> for SharedBytes {
    fn from(bytes: [u8; N]) -> Self {
        Self::from(Vec::from(bytes))
    }
}

impl<const N: usize> From<&[u8; N]> for SharedBytes {
    fn from(bytes: &[u8; N]) -> Self {
        Self::copy_from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::SharedBytes;

    #[test]
    fn supports_common_byte_conversions() {
        let from_vec: SharedBytes = vec![1, 2, 3].into();
        let from_slice: SharedBytes = [4, 5, 6].as_slice().into();
        let from_array: SharedBytes = [7, 8, 9].into();

        assert_eq!(&*from_vec, &[1, 2, 3]);
        assert_eq!(&*from_slice, &[4, 5, 6]);
        assert_eq!(&*from_array, &[7, 8, 9]);
    }
}
