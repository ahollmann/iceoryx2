// Copyright (c) 2023 Contributors to the Eclipse Foundation
//
// See the NOTICE file(s) distributed with this work for additional
// information regarding copyright ownership.
//
// This program and the accompanying materials are made available under the
// terms of the Apache Software License 2.0 which is available at
// https://www.apache.org/licenses/LICENSE-2.0, or the MIT license
// which is available at https://opensource.org/licenses/MIT.
//
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! # Example
//!
//! ## Typed API
//!
//! ```
//! use iceoryx2::prelude::*;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let node = NodeBuilder::new().create::<zero_copy::Service>()?;
//! #
//! # let service = node.service_builder("My/Funk/ServiceName".try_into()?)
//! #     .publish_subscribe::<u64>()
//! #     .open_or_create()?;
//! #
//! # let publisher = service.publisher_builder().create()?;
//!
//! let sample = publisher.loan_uninit()?;
//! // write 1234 into sample
//! let mut sample = sample.write_payload(1234);
//! // override contents with 456 because its fun
//! *sample.payload_mut() = 456;
//!
//! println!("publisher port id: {:?}", sample.header().publisher_id());
//! sample.send()?;
//!
//! # Ok(())
//! # }
//! ```
//!
//! ## Slice API
//!
//! ```
//! use iceoryx2::prelude::*;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let node = NodeBuilder::new().create::<zero_copy::Service>()?;
//! #
//! # let service = node.service_builder("My/Funk/ServiceName".try_into()?)
//! #     .publish_subscribe::<[usize]>()
//! #     .create()?;
//! #
//! # let publisher = service.publisher_builder().max_slice_len(16).create()?;
//!
//! let slice_length = 12;
//! let sample = publisher.loan_slice_uninit(slice_length)?;
//! // initialize the n-th element of the slice with n * 1234
//! let mut sample = sample.write_from_fn(|n| n * 1234);
//! // override the content of the first element with 42
//! sample.payload_mut()[0] = 42;
//!
//! println!("publisher port id: {:?}", sample.header().publisher_id());
//! sample.send()?;
//!
//! # Ok(())
//! # }
//! ```

use crate::{
    port::publisher::{DataSegment, PublisherSendError},
    raw_sample::RawSampleMut,
    service::header::publish_subscribe::Header,
};
use iceoryx2_cal::shared_memory::*;
use std::{
    fmt::{Debug, Formatter},
    mem::MaybeUninit,
    sync::Arc,
};

/// Acquired by a [`crate::port::publisher::Publisher`] via
///  * [`crate::port::publisher::Publisher::loan()`],
///  * [`crate::port::publisher::Publisher::loan_uninit()`]
///  * [`crate::port::publisher::Publisher::loan_slice()`]
///  * [`crate::port::publisher::Publisher::loan_slice_uninit()`]
///
/// It stores the payload that will be sent
/// to all connected [`crate::port::subscriber::Subscriber`]s. If the [`SampleMut`] is not sent
/// it will release the loaned memory when going out of scope.
///
/// # Notes
///
/// Does not implement [`Send`] since it releases unsent samples in the [`crate::port::publisher::Publisher`] and the
/// [`crate::port::publisher::Publisher`] is not thread-safe!
///
/// The generic parameter `M` is either a `PayloadType` or a [`core::mem::MaybeUninit<PayloadType>`], depending
/// which API is used to obtain the sample.
pub struct SampleMut<PayloadType: Debug + ?Sized, Service: crate::service::Service> {
    data_segment: Arc<DataSegment<Service>>,
    ptr: RawSampleMut<Header, PayloadType>,
    pub(crate) offset_to_chunk: PointerOffset,
}

impl<PayloadType: Debug + ?Sized, Service: crate::service::Service> Debug
    for SampleMut<PayloadType, Service>
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SampleMut<{}, {}> {{ data_segment: {:?}, offset_to_chunk: {:?} }}",
            core::any::type_name::<PayloadType>(),
            core::any::type_name::<Service>(),
            self.data_segment,
            self.offset_to_chunk
        )
    }
}

impl<PayloadType: Debug + ?Sized, Service: crate::service::Service> Drop
    for SampleMut<PayloadType, Service>
{
    fn drop(&mut self) {
        self.data_segment.return_loaned_sample(self.offset_to_chunk);
    }
}

impl<PayloadType: Debug, Service: crate::service::Service>
    SampleMut<MaybeUninit<PayloadType>, Service>
{
    pub(crate) fn new(
        data_segment: &Arc<DataSegment<Service>>,
        ptr: RawSampleMut<Header, MaybeUninit<PayloadType>>,
        offset_to_chunk: PointerOffset,
    ) -> Self {
        Self {
            data_segment: Arc::clone(data_segment),
            ptr,
            offset_to_chunk,
        }
    }
}

impl<PayloadType: Debug, Service: crate::service::Service>
    SampleMut<[MaybeUninit<PayloadType>], Service>
{
    pub(crate) fn new(
        data_segment: &Arc<DataSegment<Service>>,
        ptr: RawSampleMut<Header, [MaybeUninit<PayloadType>]>,
        offset_to_chunk: PointerOffset,
    ) -> Self {
        Self {
            data_segment: Arc::clone(data_segment),
            ptr,
            offset_to_chunk,
        }
    }
}

impl<PayloadType: Debug, Service: crate::service::Service>
    SampleMut<MaybeUninit<PayloadType>, Service>
{
    /// Writes the payload to the sample and labels the sample as initialized
    ///
    /// # Example
    ///
    /// ```
    /// use iceoryx2::prelude::*;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let node = NodeBuilder::new().create::<zero_copy::Service>()?;
    /// #
    /// # let service = node.service_builder("My/Funk/ServiceName".try_into()?)
    /// #     .publish_subscribe::<u64>()
    /// #     .open_or_create()?;
    /// #
    /// # let publisher = service.publisher_builder().create()?;
    ///
    /// let sample = publisher.loan_uninit()?;
    /// let sample = sample.write_payload(1234);
    ///
    /// sample.send()?;
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn write_payload(mut self, value: PayloadType) -> SampleMut<PayloadType, Service> {
        self.payload_mut().write(value);
        // SAFETY: this is safe since the payload was initialized on the line above
        unsafe { self.assume_init() }
    }

    /// Extracts the value of the [`core::mem::MaybeUninit<PayloadType>`] container and labels the sample as initialized
    ///
    /// # Safety
    ///
    /// The caller must ensure that [`core::mem::MaybeUninit<PayloadType>`] really is initialized. Calling this when
    /// the content is not fully initialized causes immediate undefined behavior.
    ///
    /// # Example
    ///
    /// ```
    /// use iceoryx2::prelude::*;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let node = NodeBuilder::new().create::<zero_copy::Service>()?;
    /// #
    /// # let service = node.service_builder("My/Funk/ServiceName".try_into()?)
    /// #     .publish_subscribe::<u64>()
    /// #     .open_or_create()?;
    /// #
    /// # let publisher = service.publisher_builder().create()?;
    ///
    /// let mut sample = publisher.loan_uninit()?;
    /// sample.payload_mut().write(1234);
    /// let sample = unsafe { sample.assume_init() };
    ///
    /// sample.send()?;
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub unsafe fn assume_init(self) -> SampleMut<PayloadType, Service> {
        // the transmute is not nice but safe since MaybeUninit is #[repr(transparent)] to the inner type
        std::mem::transmute(self)
    }
}

impl<PayloadType: Debug, Service: crate::service::Service>
    SampleMut<[MaybeUninit<PayloadType>], Service>
{
    /// Extracts the value of the slice of [`core::mem::MaybeUninit<PayloadType>`] and labels the sample as initialized
    ///
    /// # Safety
    ///
    /// The caller must ensure that every element of the slice of [`core::mem::MaybeUninit<PayloadType>`]
    /// is initialized. Calling this when the content is not fully initialized causes immediate undefined behavior.
    ///
    /// # Example
    ///
    /// ```
    /// use iceoryx2::prelude::*;
    /// use core::mem::MaybeUninit;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let node = NodeBuilder::new().create::<zero_copy::Service>()?;
    /// #
    /// # let service = node.service_builder("My/Funk/ServiceName".try_into()?)
    /// #     .publish_subscribe::<[usize]>()
    /// #     .open_or_create()?;
    /// #
    /// # let publisher = service.publisher_builder().max_slice_len(32).create()?;
    ///
    /// let slice_length = 10;
    /// let mut sample = publisher.loan_slice_uninit(slice_length)?;
    ///
    /// for element in sample.payload_mut() {
    ///     element.write(1234);
    /// }
    ///
    /// let sample = unsafe { sample.assume_init() };
    ///
    /// sample.send()?;
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub unsafe fn assume_init(self) -> SampleMut<[PayloadType], Service> {
        // the transmute is not nice but safe since MaybeUninit is #[repr(transparent)] to the inner type
        std::mem::transmute(self)
    }

    /// Writes the payload to the sample and labels the sample as initialized
    ///
    /// # Example
    ///
    /// ```
    /// use iceoryx2::prelude::*;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let node = NodeBuilder::new().create::<zero_copy::Service>()?;
    /// #
    /// # let service = node.service_builder("My/Funk/ServiceName".try_into()?)
    /// #     .publish_subscribe::<[usize]>()
    /// #     .open_or_create()?;
    /// #
    /// # let publisher = service.publisher_builder().max_slice_len(16).create()?;
    ///
    /// let slice_length = 12;
    /// let sample = publisher.loan_slice_uninit(slice_length)?;
    /// let sample = sample.write_from_fn(|n| n + 123);
    ///
    /// sample.send()?;
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn write_from_fn<F: FnMut(usize) -> PayloadType>(
        mut self,
        mut initializer: F,
    ) -> SampleMut<[PayloadType], Service> {
        for (i, element) in self.payload_mut().iter_mut().enumerate() {
            element.write(initializer(i));
        }

        // SAFETY: this is safe since the payload was initialized on the line above
        unsafe { self.assume_init() }
    }
}

impl<
        M: Debug + ?Sized, // `M` is either a `PayloadType` or a `MaybeUninit<PayloadType>`
        Service: crate::service::Service,
    > SampleMut<M, Service>
{
    /// Returns a reference to the header of the sample.
    ///
    /// # Example
    ///
    /// ```
    /// use iceoryx2::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let node = NodeBuilder::new().create::<zero_copy::Service>()?;
    /// #
    /// # let service = node.service_builder("My/Funk/ServiceName".try_into()?)
    /// #     .publish_subscribe::<u64>()
    /// #     .open_or_create()?;
    /// # let publisher = service.publisher_builder().create()?;
    ///
    /// let sample = publisher.loan()?;
    /// println!("Sample Publisher Origin {:?}", sample.header().publisher_id());
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn header(&self) -> &Header {
        self.ptr.as_header_ref()
    }

    /// Returns a reference to the payload of the sample.
    ///
    /// # Notes
    ///
    /// The generic parameter `PayloadType` can be packed into [`core::mem::MaybeUninit<PayloadType>`], depending
    /// which API is used to obtain the sample. Obtaining a reference is safe for either type.
    ///
    /// # Example
    ///
    /// ```
    /// use iceoryx2::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let node = NodeBuilder::new().create::<zero_copy::Service>()?;
    /// #
    /// # let service = node.service_builder("My/Funk/ServiceName".try_into()?)
    /// #     .publish_subscribe::<u64>()
    /// #     .open_or_create()?;
    /// # let publisher = service.publisher_builder().create()?;
    ///
    /// let sample = publisher.loan()?;
    /// println!("Sample current payload {}", sample.payload());
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn payload(&self) -> &M {
        self.ptr.as_payload_ref()
    }

    /// Returns a mutable reference to the payload of the sample.
    ///
    /// # Notes
    ///
    /// The generic parameter `PayloadType` can be packed into [`core::mem::MaybeUninit<PayloadType>`], depending
    /// which API is used to obtain the sample. Obtaining a reference is safe for either type.
    ///
    /// # Example
    ///
    /// ```
    /// use iceoryx2::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let node = NodeBuilder::new().create::<zero_copy::Service>()?;
    /// #
    /// # let service = node.service_builder("My/Funk/ServiceName".try_into()?)
    /// #     .publish_subscribe::<u64>()
    /// #     .open_or_create()?;
    /// # let publisher = service.publisher_builder().create()?;
    ///
    /// let mut sample = publisher.loan()?;
    /// *sample.payload_mut() = 4567;
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn payload_mut(&mut self) -> &mut M {
        self.ptr.as_payload_mut()
    }

    /// Send a previously loaned [`crate::port::publisher::Publisher::loan_uninit()`] or
    /// [`crate::port::publisher::Publisher::loan()`] [`SampleMut`] to all connected
    /// [`crate::port::subscriber::Subscriber`]s of the service.
    ///
    /// The payload of the [`SampleMut`] must be initialized before it can be sent. Have a look
    /// at [`SampleMut::write_payload()`] and [`SampleMut::assume_init()`]
    /// for more details.
    ///
    /// On success the number of [`crate::port::subscriber::Subscriber`]s that received
    /// the data is returned, otherwise a [`PublisherSendError`] describing the failure.
    ///
    /// # Example
    ///
    /// ```
    /// use iceoryx2::prelude::*;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let node = NodeBuilder::new().create::<zero_copy::Service>()?;
    /// #
    /// # let service = node.service_builder("My/Funk/ServiceName".try_into()?)
    /// #     .publish_subscribe::<u64>()
    /// #     .open_or_create()?;
    /// # let publisher = service.publisher_builder().create()?;
    ///
    /// let mut sample = publisher.loan()?;
    /// *sample.payload_mut() = 4567;
    ///
    /// sample.send()?;
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn send(self) -> Result<usize, PublisherSendError> {
        self.data_segment.send_sample(self.offset_to_chunk.value())
    }
}
