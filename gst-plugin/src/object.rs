// Copyright (C) 2017 Sebastian Dröge <sebastian@centricular.com>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::ffi::CString;
use std::ptr;
use std::mem;
use std::sync::atomic::AtomicBool;
use std::any::TypeId;
use std::collections::BTreeMap;
use std::sync::Mutex;

use glib_ffi;
use gobject_ffi;

use glib;
use glib::translate::*;

use properties::*;

pub trait ObjectImpl<T: ObjectType>: Send + Sync + 'static {
    fn set_property(&self, _obj: &glib::Object, _id: u32, _value: &glib::Value) {
        unimplemented!()
    }

    fn get_property(&self, _obj: &glib::Object, _id: u32) -> Result<glib::Value, ()> {
        unimplemented!()
    }

    fn notify(&self, obj: &glib::Object, name: &str) {
        unsafe {
            gobject_ffi::g_object_notify(obj.to_glib_none().0, name.to_glib_none().0);
        }
    }
}

#[macro_export]
macro_rules! box_object_impl(
    ($name:ident) => {
        impl<T: ObjectType> ObjectImpl<T> for Box<$name<T>> {
            fn set_property(&self, obj: &glib::Object, id: u32, value: &glib::Value) {
                let imp: &$name<T> = self.as_ref();
                imp.set_property(obj, id, value);
            }

            fn get_property(&self, obj: &glib::Object, id: u32) -> Result<glib::Value, ()> {
                let imp: &$name<T> = self.as_ref();
                imp.get_property(obj, id)
            }
        }
    };
);

pub trait ImplTypeStatic<T: ObjectType>: Send + Sync + 'static {
    fn get_name(&self) -> &str;
    fn new(&self, &T) -> T::ImplType;
    fn class_init(&self, &mut ClassStruct<T>);
    fn type_init(&self, _: &TypeInitToken, _type_: glib::Type) {}
}

pub struct ClassInitToken(());
pub struct TypeInitToken(());

pub trait ObjectType: FromGlibPtrBorrow<*mut InstanceStruct<Self>> + 'static
where
    Self: Sized,
{
    const NAME: &'static str;
    type GlibType;
    type GlibClassType;
    type ImplType: ObjectImpl<Self>;

    fn glib_type() -> glib::Type;

    fn class_init(token: &ClassInitToken, klass: &mut ClassStruct<Self>);

    fn set_property(_obj: &Self, _id: u32, _value: &glib::Value) {
        unimplemented!()
    }

    fn get_property(_obj: &Self, _id: u32) -> Result<glib::Value, ()> {
        unimplemented!()
    }

    unsafe fn get_instance(&self) -> *mut InstanceStruct<Self>;

    fn get_impl(&self) -> &Self::ImplType {
        unsafe { (*self.get_instance()).get_impl() }
    }

    unsafe fn get_class(&self) -> *const ClassStruct<Self> {
        (*self.get_instance()).get_class()
    }
}

#[macro_export]
macro_rules! object_type_fns(
    () => {
        unsafe fn get_instance(&self) -> *mut InstanceStruct<Self> {
            self.to_glib_none().0
        }
    }
);

#[repr(C)]
pub struct InstanceStruct<T: ObjectType> {
    pub parent: T::GlibType,
    pub imp: *const T::ImplType,
    pub panicked: AtomicBool,
}

impl<T: ObjectType> InstanceStruct<T> {
    pub fn get_impl(&self) -> &T::ImplType {
        unsafe { &*self.imp }
    }

    pub unsafe fn get_class(&self) -> *const ClassStruct<T> {
        *(self as *const _ as *const *const ClassStruct<T>)
    }
}

#[repr(C)]
pub struct ClassStruct<T: ObjectType> {
    pub parent: T::GlibClassType,
    pub imp_static: *const Box<ImplTypeStatic<T>>,
    pub parent_class: *const T::GlibClassType,
    pub interfaces_static: *const Vec<(glib_ffi::GType, glib_ffi::gpointer)>,
}

impl<T: ObjectType> ClassStruct<T> {
    pub unsafe fn get_parent_class(&self) -> *const T::GlibClassType {
        self.parent_class
    }
}

impl<T: ObjectType> ClassStruct<T> {
    pub fn get_interface_static(&self, type_: glib_ffi::GType) -> glib_ffi::gpointer {
        unsafe {
            if self.interfaces_static.is_null() {
                return ptr::null_mut();
            }

            for &(t, p) in &(*self.interfaces_static) {
                if t == type_ {
                    return p;
                }
            }

            ptr::null_mut()
        }
    }
}

pub unsafe trait ObjectClass {
    fn install_properties(&mut self, properties: &[Property]) {
        if properties.is_empty() {
            return;
        }

        let mut pspecs = Vec::with_capacity(properties.len());

        pspecs.push(ptr::null_mut());

        for property in properties {
            pspecs.push(property.into());
        }

        unsafe {
            gobject_ffi::g_object_class_install_properties(
                self as *mut _ as *mut gobject_ffi::GObjectClass,
                pspecs.len() as u32,
                pspecs.as_mut_ptr(),
            );
        }
    }
}

unsafe impl<T: ObjectType> ObjectClass for ClassStruct<T> {}

unsafe extern "C" fn class_init<T: ObjectType>(
    klass: glib_ffi::gpointer,
    _klass_data: glib_ffi::gpointer,
) {
    callback_guard!();
    {
        let gobject_klass = &mut *(klass as *mut gobject_ffi::GObjectClass);

        gobject_klass.finalize = Some(finalize::<T>);
        gobject_klass.set_property = Some(set_property::<T>);
        gobject_klass.get_property = Some(get_property::<T>);
    }

    {
        let klass = &mut *(klass as *mut ClassStruct<T>);
        klass.parent_class = gobject_ffi::g_type_class_peek_parent(
            klass as *mut _ as glib_ffi::gpointer,
        ) as *const T::GlibClassType;
        T::class_init(&ClassInitToken(()), klass);
    }
}

unsafe extern "C" fn finalize<T: ObjectType>(obj: *mut gobject_ffi::GObject) {
    callback_guard!();
    let instance = &mut *(obj as *mut InstanceStruct<T>);

    drop(Box::from_raw(instance.imp as *mut T::ImplType));
    instance.imp = ptr::null_mut();

    let klass = *(obj as *const glib_ffi::gpointer);
    let parent_klass = gobject_ffi::g_type_class_peek_parent(klass);
    let parent_klass =
        &*(gobject_ffi::g_type_class_peek_parent(parent_klass) as *const gobject_ffi::GObjectClass);
    parent_klass.finalize.map(|f| f(obj));
}

unsafe extern "C" fn get_property<T: ObjectType>(
    obj: *mut gobject_ffi::GObject,
    id: u32,
    value: *mut gobject_ffi::GValue,
    _pspec: *mut gobject_ffi::GParamSpec,
) {
    callback_guard!();
    floating_reference_guard!(obj);
    match T::get_property(&from_glib_borrow(obj as *mut InstanceStruct<T>), id - 1) {
        Ok(v) => {
            gobject_ffi::g_value_unset(value);
            ptr::write(value, ptr::read(v.to_glib_none().0));
            mem::forget(v);
        }
        Err(_) => unimplemented!(),
    }
}

unsafe extern "C" fn set_property<T: ObjectType>(
    obj: *mut gobject_ffi::GObject,
    id: u32,
    value: *mut gobject_ffi::GValue,
    _pspec: *mut gobject_ffi::GParamSpec,
) {
    callback_guard!();
    floating_reference_guard!(obj);
    T::set_property(
        &from_glib_borrow(obj as *mut InstanceStruct<T>),
        id - 1,
        &*(value as *mut glib::Value),
    );
}

static mut TYPES: *mut Mutex<BTreeMap<TypeId, glib::Type>> = 0 as *mut _;

pub unsafe fn get_type<T: ObjectType>() -> glib_ffi::GType {
    use std::sync::{Once, ONCE_INIT};

    static ONCE: Once = ONCE_INIT;

    ONCE.call_once(|| {
        TYPES = Box::into_raw(Box::new(Mutex::new(BTreeMap::new())));
    });

    let mut types = (*TYPES).lock().unwrap();
    types
        .entry(TypeId::of::<T>())
        .or_insert_with(|| {
            let type_info = gobject_ffi::GTypeInfo {
                class_size: mem::size_of::<ClassStruct<T>>() as u16,
                base_init: None,
                base_finalize: None,
                class_init: Some(class_init::<T>),
                class_finalize: None,
                class_data: ptr::null_mut(),
                instance_size: mem::size_of::<InstanceStruct<T>>() as u16,
                n_preallocs: 0,
                instance_init: None,
                value_table: ptr::null(),
            };

            let type_name = {
                let mut idx = 0;

                loop {
                    let type_name = CString::new(format!("{}-{}", T::NAME, idx)).unwrap();
                    if gobject_ffi::g_type_from_name(type_name.as_ptr())
                        == gobject_ffi::G_TYPE_INVALID
                    {
                        break type_name;
                    }
                    idx += 1;
                }
            };

            from_glib(gobject_ffi::g_type_register_static(
                T::glib_type().to_glib(),
                type_name.as_ptr(),
                &type_info,
                gobject_ffi::G_TYPE_FLAG_ABSTRACT,
            ))
        })
        .to_glib()
}

unsafe extern "C" fn sub_class_init<T: ObjectType>(
    klass: glib_ffi::gpointer,
    klass_data: glib_ffi::gpointer,
) {
    callback_guard!();
    {
        let gobject_klass = &mut *(klass as *mut gobject_ffi::GObjectClass);

        gobject_klass.set_property = Some(sub_set_property::<T>);
        gobject_klass.get_property = Some(sub_get_property::<T>);
    }
    {
        let klass = &mut *(klass as *mut ClassStruct<T>);
        let imp_static = klass_data as *const Box<ImplTypeStatic<T>>;
        klass.imp_static = imp_static;
        klass.interfaces_static = Box::into_raw(Box::new(Vec::new()));

        (*imp_static).class_init(klass);
    }
}

unsafe extern "C" fn sub_get_property<T: ObjectType>(
    obj: *mut gobject_ffi::GObject,
    id: u32,
    value: *mut gobject_ffi::GValue,
    _pspec: *mut gobject_ffi::GParamSpec,
) {
    callback_guard!();
    floating_reference_guard!(obj);
    let instance = &*(obj as *mut InstanceStruct<T>);
    let imp = instance.get_impl();

    match imp.get_property(&from_glib_borrow(obj), id - 1) {
        Ok(v) => {
            gobject_ffi::g_value_unset(value);
            ptr::write(value, ptr::read(v.to_glib_none().0));
            mem::forget(v);
        }
        Err(_) => unimplemented!(),
    }
}

unsafe extern "C" fn sub_set_property<T: ObjectType>(
    obj: *mut gobject_ffi::GObject,
    id: u32,
    value: *mut gobject_ffi::GValue,
    _pspec: *mut gobject_ffi::GParamSpec,
) {
    callback_guard!();
    floating_reference_guard!(obj);
    let instance = &*(obj as *mut InstanceStruct<T>);
    let imp = instance.get_impl();
    imp.set_property(
        &from_glib_borrow(obj),
        id - 1,
        &*(value as *mut glib::Value),
    );
}

unsafe extern "C" fn sub_init<T: ObjectType>(
    obj: *mut gobject_ffi::GTypeInstance,
    _klass: glib_ffi::gpointer,
) {
    callback_guard!();
    floating_reference_guard!(obj);
    let instance = &mut *(obj as *mut InstanceStruct<T>);
    let klass = &**(obj as *const *const ClassStruct<T>);
    let rs_instance: T = from_glib_borrow(obj as *mut InstanceStruct<T>);

    let imp = (*klass.imp_static).new(&rs_instance);
    instance.imp = Box::into_raw(Box::new(imp));
}

pub fn register_type<T: ObjectType, I: ImplTypeStatic<T>>(imp: I) -> glib::Type {
    unsafe {
        let parent_type = get_type::<T>();
        let type_name = format!("{}-{}", T::NAME, imp.get_name());

        let imp: Box<ImplTypeStatic<T>> = Box::new(imp);
        let imp_ptr = Box::into_raw(Box::new(imp));

        let type_info = gobject_ffi::GTypeInfo {
            class_size: mem::size_of::<ClassStruct<T>>() as u16,
            base_init: None,
            base_finalize: None,
            class_init: Some(sub_class_init::<T>),
            class_finalize: None,
            class_data: imp_ptr as glib_ffi::gpointer,
            instance_size: mem::size_of::<InstanceStruct<T>>() as u16,
            n_preallocs: 0,
            instance_init: Some(sub_init::<T>),
            value_table: ptr::null(),
        };

        let type_ = from_glib(gobject_ffi::g_type_register_static(
            parent_type,
            type_name.to_glib_none().0,
            &type_info,
            gobject_ffi::GTypeFlags::empty(),
        ));

        (*imp_ptr).type_init(&TypeInitToken(()), type_);

        type_
    }
}
