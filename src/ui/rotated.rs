//! A paintable that shows another one turned by quarter turns, so the
//! camera preview can be turned without restarting the camera.

use std::cell::{Cell, RefCell};

use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RotatedPaintable {
        pub inner: RefCell<Option<gdk::Paintable>>,
        pub handlers: RefCell<Vec<glib::SignalHandlerId>>,
        /// Clockwise quarter turns, 0 to 3.
        pub turns: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RotatedPaintable {
        const NAME: &'static str = "SanctumRotatedPaintable";
        type Type = super::RotatedPaintable;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for RotatedPaintable {
        fn dispose(&self) {
            if let Some(inner) = self.inner.take() {
                for handler in self.handlers.take() {
                    inner.disconnect(handler);
                }
            }
        }
    }

    impl PaintableImpl for RotatedPaintable {
        fn intrinsic_width(&self) -> i32 {
            let inner = self.inner.borrow();
            let Some(inner) = inner.as_ref() else {
                return 0;
            };
            if self.turns.get() % 2 == 1 {
                inner.intrinsic_height()
            } else {
                inner.intrinsic_width()
            }
        }

        fn intrinsic_height(&self) -> i32 {
            let inner = self.inner.borrow();
            let Some(inner) = inner.as_ref() else {
                return 0;
            };
            if self.turns.get() % 2 == 1 {
                inner.intrinsic_width()
            } else {
                inner.intrinsic_height()
            }
        }

        fn intrinsic_aspect_ratio(&self) -> f64 {
            let inner = self.inner.borrow();
            let Some(inner) = inner.as_ref() else {
                return 0.0;
            };
            let ratio = inner.intrinsic_aspect_ratio();
            if self.turns.get() % 2 == 1 && ratio > 0.0 {
                1.0 / ratio
            } else {
                ratio
            }
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let inner = self.inner.borrow();
            let Some(inner) = inner.as_ref() else { return };
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else {
                return;
            };
            let turns = self.turns.get() % 4;
            let (w, h) = (width as f32, height as f32);
            snapshot.save();
            match turns {
                1 => snapshot.translate(&graphene::Point::new(w, 0.0)),
                2 => snapshot.translate(&graphene::Point::new(w, h)),
                3 => snapshot.translate(&graphene::Point::new(0.0, h)),
                _ => {}
            }
            snapshot.rotate(90.0 * turns as f32);
            if turns % 2 == 1 {
                inner.snapshot(snapshot, height, width);
            } else {
                inner.snapshot(snapshot, width, height);
            }
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct RotatedPaintable(ObjectSubclass<imp::RotatedPaintable>)
        @implements gdk::Paintable;
}

impl RotatedPaintable {
    pub fn new(inner: &gdk::Paintable, turns: u32) -> Self {
        let this: Self = glib::Object::new();
        let imp = this.imp();
        imp.turns.set(turns % 4);
        let weak = this.downgrade();
        let contents = inner.connect_invalidate_contents(move |_| {
            if let Some(this) = weak.upgrade() {
                this.invalidate_contents();
            }
        });
        let weak = this.downgrade();
        let size = inner.connect_invalidate_size(move |_| {
            if let Some(this) = weak.upgrade() {
                this.invalidate_size();
            }
        });
        imp.handlers.replace(vec![contents, size]);
        imp.inner.replace(Some(inner.clone()));
        this
    }

    pub fn set_turns(&self, turns: u32) {
        self.imp().turns.set(turns % 4);
        self.invalidate_size();
        self.invalidate_contents();
    }
}
